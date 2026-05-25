import QtQuick
import QtQuick.Layouts
import Quickshell.Io

RowLayout {
    id: root
    spacing: 16
    layoutDirection: Qt.RightToLeft

    signal settingsRequested()

    property string ipAddress: "..."
    readonly property int _iconSize: 64

    Process {
        id: ipProcess
        command: ["hostname", "-I"]
        stdout: SplitParser {
            onRead: (line) => { root.ipAddress = line.trim().split(" ")[0] || "No IP" }
        }
    }

    Timer {
        interval: 30000
        running: true
        repeat: true
        triggeredOnStart: true
        onTriggered: { if (!ipProcess.running) ipProcess.running = true }
    }

    // Settings
    Rectangle {
        Layout.preferredWidth: root._iconSize
        Layout.preferredHeight: root._iconSize
        radius: root._iconSize / 2
        color: settingsMA.containsMouse ? Theme.surfaceHover : "transparent"
        Behavior on color { ColorAnimation { duration: 150 } }

        Text {
            anchors.centerIn: parent
            text: "⚙"
            font.pixelSize: Theme.fontBody
            color: settingsMA.containsMouse ? Theme.textPrimary : Theme.textMuted
        }

        MouseArea {
            id: settingsMA
            anchors.fill: parent
            hoverEnabled: true
            cursorShape: Qt.PointingHandCursor
            onClicked: root.settingsRequested()
        }
    }

    // Theme toggle
    Rectangle {
        Layout.preferredWidth: root._iconSize
        Layout.preferredHeight: root._iconSize
        radius: root._iconSize / 2
        color: themeMA.containsMouse ? Theme.surfaceHover : "transparent"
        Behavior on color { ColorAnimation { duration: 150 } }

        Text {
            anchors.centerIn: parent
            text: Theme.themeMode === "dark" ? "☽" :
                  Theme.themeMode === "light" ? "☀" : "◐"
            font.pixelSize: Theme.fontBody
            color: themeMA.containsMouse ? Theme.textPrimary : Theme.textMuted
        }

        MouseArea {
            id: themeMA
            anchors.fill: parent
            hoverEnabled: true
            cursorShape: Qt.PointingHandCursor
            onClicked: {
                if (Theme.themeMode === "auto") Theme.setThemeMode("light")
                else if (Theme.themeMode === "light") Theme.setThemeMode("dark")
                else Theme.setThemeMode("auto")
            }
        }
    }

    // Network
    Rectangle {
        Layout.preferredWidth: root._iconSize
        Layout.preferredHeight: root._iconSize
        radius: root._iconSize / 2
        color: "transparent"

        Text {
            anchors.centerIn: parent
            text: root.ipAddress !== "..." && root.ipAddress !== "No IP" ? "⛁" : "⚠"
            font.pixelSize: Theme.fontBody
            color: root.ipAddress !== "..." && root.ipAddress !== "No IP"
                       ? Theme.textMuted : Theme.warning
        }
    }

    // Volume
    Rectangle {
        Layout.preferredWidth: root._iconSize
        Layout.preferredHeight: root._iconSize
        radius: root._iconSize / 2
        color: "transparent"

        Text {
            anchors.centerIn: parent
            text: "♫"
            font.pixelSize: Theme.fontBody
            color: Theme.textMuted
        }
    }
}
