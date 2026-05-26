import QtQuick
import QtQuick.Layouts
import Quickshell.Io

FocusScope {
    id: root

    property var monitors: []
    property int selectedMonitor: 0

    // --- Processes ---

    Process {
        id: getMonitors
        command: ["hyprctl", "monitors", "-j"]
        stdout: SplitParser {
            property string buffer: ""
            onRead: (line) => { buffer += line }
        }
        onExited: {
            try {
                let data = JSON.parse(getMonitors.stdout.buffer)
                let mons = []
                for (let i = 0; i < data.length; i++) {
                    let m = data[i]
                    mons.push({
                        name: m.name || "Unknown",
                        description: m.description || "",
                        width: m.width || 0,
                        height: m.height || 0,
                        refreshRate: m.refreshRate || 0,
                        scale: m.scale || 1.0,
                        x: m.x || 0,
                        y: m.y || 0,
                        activeWorkspace: m.activeWorkspace ? m.activeWorkspace.name : "",
                        dpmsStatus: m.dpmsStatus !== false,
                        vrr: m.vrr || false,
                        availableModes: m.availableModes || []
                    })
                }
                root.monitors = mons
            } catch(e) {
                console.log("Failed to parse monitor data:", e)
            }
            getMonitors.stdout.buffer = ""
        }
    }

    Process {
        id: setScale
        property string monName: ""
        property real scaleVal: 1.0
        command: ["hyprctl", "keyword", "monitor", monName + ",preferred,auto," + scaleVal]
        onExited: { getMonitors.running = true }
    }

    Process {
        id: setMode
        property string monName: ""
        property string mode: ""
        command: ["hyprctl", "keyword", "monitor", monName + "," + mode + ",auto,1"]
        onExited: { getMonitors.running = true }
    }

    Component.onCompleted: { getMonitors.running = true }

    onVisibleChanged: {
        if (visible) {
            getMonitors.running = true
            monitorList.forceActiveFocus()
        }
    }

    ColumnLayout {
        anchors.fill: parent
        anchors.margins: Theme.padding
        spacing: 32

        Text {
            text: "Displays"
            font.pixelSize: Theme.fontBody
            font.bold: true
            color: Theme.textPrimary
        }

        // Monitor list
        ListView {
            id: monitorList
            Layout.fillWidth: true
            Layout.preferredHeight: Math.min(root.monitors.length * 200, 400)
            spacing: 16
            clip: true
            model: root.monitors
            focus: true

            KeyNavigation.down: modeList

            delegate: Rectangle {
                required property int index
                required property var modelData
                width: monitorList.width
                height: 180
                radius: 16
                color: monitorList.currentIndex === index && monitorList.activeFocus
                       ? Theme.surfaceHover : Theme.surface
                border.width: 2
                border.color: Theme.surfaceBorder

                Behavior on color { ColorAnimation { duration: 150 } }

                property bool isSel: monitorList.currentIndex === index && monitorList.activeFocus

                ColumnLayout {
                    anchors.fill: parent
                    anchors.leftMargin: 24
                    anchors.rightMargin: 24
                    anchors.topMargin: 20
                    anchors.bottomMargin: 20
                    spacing: 8

                    RowLayout {
                        Layout.fillWidth: true
                        spacing: 16

                        Text {
                            text: modelData.name
                            font.pixelSize: Theme.fontBody
                            font.bold: true
                            color: Theme.textPrimary
                        }

                        Text {
                            text: modelData.description
                            font.pixelSize: Theme.fontSmall
                            color: Theme.textSecondary
                            elide: Text.ElideRight
                            Layout.fillWidth: true
                        }
                    }

                    RowLayout {
                        spacing: 32

                        Text {
                            text: "Resolution: " + modelData.width + "x" + modelData.height
                            font.pixelSize: Theme.fontSmall
                            color: Theme.textSecondary
                        }

                        Text {
                            text: "Refresh: " + modelData.refreshRate.toFixed(1) + " Hz"
                            font.pixelSize: Theme.fontSmall
                            color: Theme.textSecondary
                        }

                        Text {
                            text: "Scale: " + modelData.scale.toFixed(1) + "x"
                            font.pixelSize: Theme.fontSmall
                            color: Theme.textSecondary
                        }
                    }

                    RowLayout {
                        spacing: 32

                        Text {
                            text: "Position: " + modelData.x + "," + modelData.y
                            font.pixelSize: Theme.fontSmall
                            color: Theme.textSecondary
                        }

                        Text {
                            text: "DPMS: " + (modelData.dpmsStatus ? "On" : "Off")
                            font.pixelSize: Theme.fontSmall
                            color: Theme.textSecondary
                        }

                        Text {
                            text: "VRR: " + (modelData.vrr ? "On" : "Off")
                            font.pixelSize: Theme.fontSmall
                            color: Theme.textSecondary
                        }
                    }
                }

                MouseArea {
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: {
                        monitorList.currentIndex = index
                        monitorList.forceActiveFocus()
                        root.selectedMonitor = index
                    }
                }
            }

            Keys.onReturnPressed: {
                root.selectedMonitor = currentIndex
                modeList.forceActiveFocus()
            }
        }

        // Scale controls
        Text {
            text: "Scale"
            font.pixelSize: Theme.fontBody
            font.bold: true
            color: Theme.textPrimary
            visible: root.monitors.length > 0
        }

        RowLayout {
            Layout.fillWidth: true
            spacing: 16
            visible: root.monitors.length > 0

            Repeater {
                model: [0.5, 1.0, 1.25, 1.5, 2.0]

                FocusScope {
                    id: scaleScope
                    required property var modelData
                    required property int index
                    width: scaleBtn.width
                    height: scaleBtn.height
                    activeFocusOnTab: true

                    SettingsButton {
                        id: scaleBtn
                        text: parent.modelData + "x"
                        focus: parent.activeFocus
                        anchors.fill: parent

                        // Highlight current scale
                        property bool isCurrent: root.monitors.length > root.selectedMonitor &&
                                                 Math.abs(root.monitors[root.selectedMonitor].scale - parent.modelData) < 0.05

                        color: isCurrent ? Theme.sidebarActive :
                               parent.activeFocus ? Theme.surfaceHover : Theme.surface

                        MouseArea {
                            anchors.fill: parent
                            hoverEnabled: true
                            cursorShape: Qt.PointingHandCursor
                            onClicked: {
                                scaleScope.forceActiveFocus()
                                if (root.monitors.length > root.selectedMonitor) {
                                    setScale.monName = root.monitors[root.selectedMonitor].name
                                    setScale.scaleVal = scaleScope.modelData
                                    setScale.running = true
                                }
                            }
                        }
                    }

                    Keys.onReturnPressed: {
                        if (root.monitors.length > root.selectedMonitor) {
                            setScale.monName = root.monitors[root.selectedMonitor].name
                            setScale.scaleVal = modelData
                            setScale.running = true
                        }
                    }
                }
            }
        }

        // Available modes
        Text {
            text: "Available Modes"
            font.pixelSize: Theme.fontBody
            font.bold: true
            color: Theme.textPrimary
            visible: root.monitors.length > 0
        }

        ListView {
            id: modeList
            Layout.fillWidth: true
            Layout.fillHeight: true
            spacing: 8
            clip: true
            visible: root.monitors.length > 0
            model: root.monitors.length > root.selectedMonitor ? root.monitors[root.selectedMonitor].availableModes : []

            KeyNavigation.up: monitorList

            delegate: Rectangle {
                required property int index
                required property var modelData
                width: modeList.width
                height: 80
                radius: 16

                property bool isCurrent: {
                    if (root.monitors.length <= root.selectedMonitor) return false
                    let mon = root.monitors[root.selectedMonitor]
                    return modelData === (mon.width + "x" + mon.height + "@" + mon.refreshRate.toFixed(6))
                }

                color: {
                    if (isCurrent) return Theme.sidebarActive
                    if (modeList.currentIndex === index && modeList.activeFocus) return Theme.surfaceHover
                    return Theme.surface
                }
                border.width: isCurrent ? 2 : 2
                border.color: isCurrent ? Theme.focusBorder : Theme.surfaceBorder

                Behavior on color { ColorAnimation { duration: 150 } }

                Text {
                    anchors.centerIn: parent
                    text: {
                        // Parse "3840x2160@59.940000" -> "3840x2160 @ 59.94 Hz"
                        let parts = modelData.split("@")
                        let res = parts[0] || modelData
                        let hz = parts.length > 1 ? parseFloat(parts[1]).toFixed(2) + " Hz" : ""
                        return res + (hz ? "  @  " + hz : "") + (isCurrent ? "  (current)" : "")
                    }
                    font.pixelSize: Theme.fontSmall
                    color: isCurrent ? Theme.textOnDark : Theme.textPrimary
                }

                MouseArea {
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: {
                        modeList.currentIndex = index
                        modeList.forceActiveFocus()
                    }
                    onDoubleClicked: {
                        if (root.monitors.length > root.selectedMonitor) {
                            setMode.monName = root.monitors[root.selectedMonitor].name
                            setMode.mode = modelData
                            setMode.running = true
                        }
                    }
                }
            }

            Keys.onReturnPressed: {
                if (currentIndex >= 0 && root.monitors.length > root.selectedMonitor) {
                    let modes = root.monitors[root.selectedMonitor].availableModes
                    if (currentIndex < modes.length) {
                        setMode.monName = root.monitors[root.selectedMonitor].name
                        setMode.mode = modes[currentIndex]
                        setMode.running = true
                    }
                }
            }
        }

        Text {
            text: "A: Select mode  |  Double-click to apply"
            font.pixelSize: Theme.fontHint
            color: Theme.textSecondary
            Layout.alignment: Qt.AlignHCenter
        }
    }
}
