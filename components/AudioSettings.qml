import QtQuick
import QtQuick.Layouts
import Quickshell.Io

FocusScope {
    id: root

    property int volume: 50
    property bool muted: false
    property var sinks: []
    property int defaultSinkIndex: -1

    // --- Processes ---

    Process {
        id: getVolume
        command: ["wpctl", "get-volume", "@DEFAULT_AUDIO_SINK@"]
        stdout: SplitParser {
            onRead: (line) => {
                // Output: "Volume: 0.50" or "Volume: 0.50 [MUTED]"
                let parts = line.trim().split(" ")
                if (parts.length >= 2) {
                    root.volume = Math.round(parseFloat(parts[1]) * 100)
                }
                root.muted = line.indexOf("[MUTED]") >= 0
            }
        }
    }

    Process {
        id: setVolume
        property string level: "50%"
        command: ["wpctl", "set-volume", "@DEFAULT_AUDIO_SINK@", level]
        onExited: { getVolume.running = true }
    }

    Process {
        id: toggleMute
        command: ["wpctl", "set-mute", "@DEFAULT_AUDIO_SINK@", "toggle"]
        onExited: { getVolume.running = true }
    }

    Process {
        id: listSinks
        command: ["bash", "-c", "wpctl status | sed -n '/Audio/,/Video/p' | sed -n '/Sinks:/,/Sources:/p' | grep -v 'Sinks:\\|Sources:\\|^$'"]
        stdout: SplitParser {
            property var collected: []
            onRead: (line) => {
                // Lines like: " │      46. Denon AVR-X1700H  [vol: 1.00]"
                //             " │  *   86. Radeon HD Audio   [vol: 1.00]"
                // Strip box-drawing chars and leading whitespace
                let cleaned = line.replace(/[│├└─┐┘┌┬┴┤┼]/g, " ")
                let isDefault = cleaned.indexOf("*") >= 0
                // Extract id and name from "  *   86. Some Name  [vol: 1.00]"
                let match = cleaned.match(/\*?\s*(\d+)\.\s+(.+?)(?:\s+\[vol:.+\])?\s*$/)
                if (match) {
                    let entry = {
                        id: parseInt(match[1]),
                        name: match[2].trim(),
                        isDefault: isDefault
                    }
                    collected.push(entry)
                    if (isDefault) root.defaultSinkIndex = collected.length - 1
                }
            }
        }
        onExited: {
            root.sinks = listSinks.stdout.collected
            listSinks.stdout.collected = []
        }
    }

    Process {
        id: setDefaultSink
        property int sinkId: 0
        command: ["wpctl", "set-default", String(sinkId)]
        onExited: {
            listSinks.running = true
            // Small delay then refresh volume for the new default
            refreshTimer.start()
        }
    }

    Timer {
        id: refreshTimer
        interval: 500
        onTriggered: { getVolume.running = true }
    }

    Component.onCompleted: {
        getVolume.running = true
        listSinks.running = true
    }

    // Refresh when section becomes visible
    onVisibleChanged: {
        if (visible) {
            getVolume.running = true
            listSinks.running = true
            volumeRow.forceActiveFocus()
        }
    }

    ColumnLayout {
        anchors.fill: parent
        anchors.margins: Theme.padding
        spacing: 32

        // Volume control
        Text {
            text: "Volume"
            font.pixelSize: Theme.fontBody
            font.bold: true
            color: Theme.textPrimary
        }

        RowLayout {
            id: volumeRow
            Layout.fillWidth: true
            spacing: 24
            focus: true

            FocusScope {
                id: volDownScope
                width: volDownBtn.width
                height: volDownBtn.height
                activeFocusOnTab: true

                KeyNavigation.right: volUpScope
                KeyNavigation.down: muteScope

                SettingsButton {
                    id: volDownBtn
                    text: "  -  "
                    focus: parent.activeFocus
                    anchors.fill: parent

                    MouseArea {
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: {
                            volDownScope.forceActiveFocus()
                            root.volume = Math.max(0, root.volume - 5)
                            setVolume.level = root.volume + "%"
                            setVolume.running = true
                        }
                    }
                }

                Keys.onReturnPressed: {
                    root.volume = Math.max(0, root.volume - 5)
                    setVolume.level = root.volume + "%"
                    setVolume.running = true
                }
            }

            // Volume bar
            Rectangle {
                Layout.fillWidth: true
                height: 56
                radius: 28
                color: Theme.surfaceHover

                Rectangle {
                    width: parent.width * (root.volume / 100)
                    height: parent.height
                    radius: 28
                    color: root.muted ? Theme.textSecondary : Theme.navy

                    Behavior on width { NumberAnimation { duration: 100 } }
                }

                Text {
                    anchors.centerIn: parent
                    text: root.muted ? "MUTED" : root.volume + "%"
                    font.pixelSize: Theme.fontBody
                    font.bold: true
                    color: root.volume > 40 ? Theme.textOnDark : Theme.textPrimary
                }
            }

            FocusScope {
                id: volUpScope
                width: volUpBtn.width
                height: volUpBtn.height
                activeFocusOnTab: true

                KeyNavigation.left: volDownScope
                KeyNavigation.down: muteScope

                SettingsButton {
                    id: volUpBtn
                    text: "  +  "
                    focus: parent.activeFocus
                    anchors.fill: parent

                    MouseArea {
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: {
                            volUpScope.forceActiveFocus()
                            root.volume = Math.min(100, root.volume + 5)
                            setVolume.level = root.volume + "%"
                            setVolume.running = true
                        }
                    }
                }

                Keys.onReturnPressed: {
                    root.volume = Math.min(100, root.volume + 5)
                    setVolume.level = root.volume + "%"
                    setVolume.running = true
                }
            }
        }

        FocusScope {
            id: muteScope
            width: muteBtn.width
            height: muteBtn.height
            activeFocusOnTab: true

            KeyNavigation.up: volDownScope
            KeyNavigation.down: sinkList

            SettingsButton {
                id: muteBtn
                text: root.muted ? "Unmute" : "Mute"
                focus: parent.activeFocus
                anchors.fill: parent

                MouseArea {
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: {
                        muteScope.forceActiveFocus()
                        toggleMute.running = true
                    }
                }
            }

            Keys.onReturnPressed: { toggleMute.running = true }
        }

        // Output devices
        Text {
            text: "Output Device"
            font.pixelSize: Theme.fontBody
            font.bold: true
            color: Theme.textPrimary
        }

        ListView {
            id: sinkList
            Layout.fillWidth: true
            Layout.fillHeight: true
            spacing: 8
            clip: true
            model: root.sinks

            KeyNavigation.up: muteScope

            delegate: Rectangle {
                required property int index
                required property var modelData
                width: sinkList.width
                height: 96
                radius: 16
                color: {
                    if (sinkList.currentIndex === index && sinkList.activeFocus)
                        return Theme.surfaceHover
                    if (modelData.isDefault)
                        return Theme.sidebarActive
                    return Theme.surface
                }
                border.width: 2
                border.color: modelData.isDefault ? Theme.focusBorder : Theme.surfaceBorder

                Behavior on color { ColorAnimation { duration: 150 } }

                RowLayout {
                    anchors.fill: parent
                    anchors.leftMargin: 24
                    anchors.rightMargin: 24
                    anchors.topMargin: 16
                    anchors.bottomMargin: 16
                    spacing: 16

                    Text {
                        text: modelData.isDefault ? ">" : " "
                        font.pixelSize: Theme.fontBody
                        color: modelData.isDefault ? Theme.textOnDark : Theme.textPrimary
                    }

                    Text {
                        text: modelData.name
                        font.pixelSize: Theme.fontSmall
                        color: modelData.isDefault ? Theme.textOnDark : Theme.textPrimary
                        elide: Text.ElideRight
                        Layout.fillWidth: true
                    }
                }

                MouseArea {
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: {
                        sinkList.currentIndex = index
                        sinkList.forceActiveFocus()
                        setDefaultSink.sinkId = modelData.id
                        setDefaultSink.running = true
                    }
                }
            }

            Keys.onReturnPressed: {
                if (currentIndex >= 0 && currentIndex < root.sinks.length) {
                    setDefaultSink.sinkId = root.sinks[currentIndex].id
                    setDefaultSink.running = true
                }
            }
        }
    }
}
