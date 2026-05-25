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

    // Resolve icon theme base path at startup
    property string _iconBase: ""

    Process {
        id: iconThemeProbe
        command: ["bash", "-c", "for d in /usr/share/icons/breeze /usr/share/icons/Adwaita /usr/share/icons/hicolor; do [ -d \"$d\" ] && echo \"$d\" && exit; done; echo ''"]
        stdout: SplitParser {
            onRead: (line) => { root._iconBase = line.trim() }
        }
    }

    Component.onCompleted: { iconThemeProbe.running = true }

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
            source: root._iconBase ? "file://" + root._iconBase + "/apps/32/preferences-system.svg" : ""
            sourceSize: Qt.size(root._imgSize, root._imgSize)
            width: root._imgSize
            height: root._imgSize
            fillMode: Image.PreserveAspectFit
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

        property string _themeIconPath: {
            if (!root._iconBase) return ""
            if (Theme.themeMode === "dark")
                return "file://" + root._iconBase + "/applets/48/weather-clear-night.svg"
            if (Theme.themeMode === "light")
                return "file://" + root._iconBase + "/applets/48/weather-clear.svg"
            return "file://" + root._iconBase + "/actions/22/brightness-high.svg"
        }

        Image {
            id: themeIcon
            anchors.centerIn: parent
            source: parent._themeIconPath
            sourceSize: Qt.size(root._imgSize, root._imgSize)
            width: root._imgSize
            height: root._imgSize
            fillMode: Image.PreserveAspectFit
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
        color: networkMA.containsMouse ? Theme.surfaceHover : "transparent"
        Behavior on color { ColorAnimation { duration: 150 } }

        property bool _connected: root.ipAddress !== "..." && root.ipAddress !== "No IP"
        property string _netIconPath: {
            if (!root._iconBase) return ""
            if (_connected)
                return "file://" + root._iconBase + "/status/22/network-wired.svg"
            return "file://" + root._iconBase + "/actions/22/network-disconnect.svg"
        }

        Image {
            id: netIcon
            anchors.centerIn: parent
            source: parent._netIconPath
            sourceSize: Qt.size(root._imgSize, root._imgSize)
            width: root._imgSize
            height: root._imgSize
            fillMode: Image.PreserveAspectFit
            visible: status === Image.Ready
        }
        Text {
            anchors.centerIn: parent
            text: parent._connected ? "⛁" : "⚠"
            font.pixelSize: root._imgSize
            color: parent._connected ? Theme.textMuted : Theme.warning
            visible: netIcon.status !== Image.Ready
        }

        MouseArea {
            id: networkMA
            anchors.fill: parent
            hoverEnabled: true
        }
    }

    // Volume
    Rectangle {
        Layout.preferredWidth: root._iconSize
        Layout.preferredHeight: root._iconSize
        radius: root._iconSize / 2
        color: volumeMA.containsMouse ? Theme.surfaceHover : "transparent"
        Behavior on color { ColorAnimation { duration: 150 } }

        Image {
            id: volIcon
            anchors.centerIn: parent
            source: root._iconBase ? "file://" + root._iconBase + "/status/22/audio-volume-high.svg" : ""
            sourceSize: Qt.size(root._imgSize, root._imgSize)
            width: root._imgSize
            height: root._imgSize
            fillMode: Image.PreserveAspectFit
            visible: status === Image.Ready
        }
        Text {
            anchors.centerIn: parent
            text: "♫"
            font.pixelSize: root._imgSize
            color: Theme.textMuted
            visible: volIcon.status !== Image.Ready
        }

        MouseArea {
            id: volumeMA
            anchors.fill: parent
            hoverEnabled: true
        }
    }
}
