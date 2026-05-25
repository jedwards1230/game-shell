import QtQuick
import QtQuick.Layouts
import Quickshell.Io

RowLayout {
    id: root
    spacing: 12
    layoutDirection: Qt.RightToLeft

    signal settingsRequested()

    property string ipAddress: "..."
    readonly property int _iconSize: 64
    readonly property int _imgSize: 32

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

        Image {
            id: settingsIcon
            anchors.centerIn: parent
            source: "image://icon/preferences-system-symbolic"
            sourceSize: Qt.size(root._imgSize, root._imgSize)
            visible: status === Image.Ready
        }
        Text {
            anchors.centerIn: parent
            text: "⚙"
            font.pixelSize: root._imgSize
            color: settingsMA.containsMouse ? Theme.textPrimary : Theme.textMuted
            visible: settingsIcon.status !== Image.Ready
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

        property string _themeIconName: Theme.themeMode === "dark"
            ? "weather-clear-night-symbolic"
            : Theme.themeMode === "light"
                ? "weather-clear-symbolic"
                : "display-brightness-symbolic"

        Image {
            id: themeIcon
            anchors.centerIn: parent
            source: "image://icon/" + parent._themeIconName
            sourceSize: Qt.size(root._imgSize, root._imgSize)
            visible: status === Image.Ready
        }
        Text {
            anchors.centerIn: parent
            text: Theme.themeMode === "dark" ? "☽"
                : Theme.themeMode === "light" ? "☀" : "◐"
            font.pixelSize: root._imgSize
            color: themeMA.containsMouse ? Theme.textPrimary : Theme.textMuted
            visible: themeIcon.status !== Image.Ready
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

        property bool _connected: root.ipAddress !== "..." && root.ipAddress !== "No IP"
        property string _netIconName: _connected
            ? "network-wired-symbolic" : "network-offline-symbolic"

        Image {
            id: netIcon
            anchors.centerIn: parent
            source: "image://icon/" + parent._netIconName
            sourceSize: Qt.size(root._imgSize, root._imgSize)
            visible: status === Image.Ready
        }
        Text {
            anchors.centerIn: parent
            text: parent._connected ? "⛁" : "⚠"
            font.pixelSize: root._imgSize
            color: parent._connected ? Theme.textMuted : Theme.warning
            visible: netIcon.status !== Image.Ready
        }
    }

    // Volume
    Rectangle {
        Layout.preferredWidth: root._iconSize
        Layout.preferredHeight: root._iconSize
        radius: root._iconSize / 2
        color: "transparent"

        Image {
            id: volIcon
            anchors.centerIn: parent
            source: "image://icon/audio-volume-high-symbolic"
            sourceSize: Qt.size(root._imgSize, root._imgSize)
            visible: status === Image.Ready
        }
        Text {
            anchors.centerIn: parent
            text: "♫"
            font.pixelSize: root._imgSize
            color: Theme.textMuted
            visible: volIcon.status !== Image.Ready
        }
    }
}
