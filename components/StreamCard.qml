import QtQuick
import Quickshell.Io

BaseCard {
    id: root

    required property var target
    property string appName: ""
    property bool isOnline: false
    property string shellState: "idle"
    property string hostActiveApp: ""
    property bool hasActiveSession: hostActiveApp !== "" && (appName === "" || hostActiveApp === appName || hostActiveApp === (target.app || ""))

    label: root.appName !== "" ? root.appName : (root.target.name || "Unknown")

    Process {
        id: pingCheck
        command: ["ping", "-c1", "-W1", root.target.host || "127.0.0.1"]
        onExited: (exitCode, exitStatus) => {
            root.isOnline = (exitCode === 0);
        }
    }

    Timer {
        interval: 10000
        running: true
        repeat: true
        triggeredOnStart: true
        onTriggered: {
            if (!pingCheck.running)
                pingCheck.running = true;
        }
    }

    Text {
        anchors.fill: parent
        text: root.appName !== "" ? root.appName.charAt(0).toUpperCase() : "\u{1F3AE}"
        font.pixelSize: 96
        font.bold: root.appName !== ""
        color: root.appName !== "" ? Theme.textSecondary : Theme.textPrimary
        horizontalAlignment: Text.AlignHCenter
        verticalAlignment: Text.AlignVCenter
    }

    Rectangle {
        id: sessionBadge
        visible: root.hasActiveSession
        height: 24
        radius: 12
        color: Theme.online
        width: badgeText.implicitWidth + 16
        anchors.top: parent.top
        anchors.right: parent.right
        anchors.topMargin: 8
        anchors.rightMargin: 8

        Text {
            id: badgeText
            anchors.centerIn: parent
            text: "LIVE"
            font.pixelSize: 12
            font.bold: true
            color: "#ffffff"
        }
    }
}
