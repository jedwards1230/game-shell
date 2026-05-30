import QtQuick
import QtQuick.Layouts
import Quickshell.Io

FocusScope {
    id: root

    property string confirmAction: ""
    // Whether logind reports suspend is available (queried via the daemon).
    // Defaults true so the Sleep button is enabled until told otherwise.
    property bool canSuspend: true

    // Suspend is routed through the input daemon's logind-over-zbus backbone
    // (Phase 3). Reboot/poweroff remain one-shot `systemctl` actions — they are
    // not system-state reads and have no daemon equivalent in scope.
    //
    // Suspend/CanSuspend go through the daemon over a native Quickshell socket
    // (SocketClient, #97) — the python3 socket shim was retired in Phase 8.

    Process {
        id: powerOff
        command: ["systemctl", "poweroff"]
    }
    Process {
        id: rebootCmd
        command: ["systemctl", "reboot"]
    }
    // logind Suspend (false = no interactive polkit prompt) via the daemon.
    SocketClient {
        id: suspendCmd
    }
    // Query logind CanSuspend so the Sleep button reflects availability.
    SocketClient {
        id: canSuspendProc
        onResponseReceived: response => {
            let t = response.trim();
            if (t === "yes")
                root.canSuspend = true;
            else if (t === "no")
                root.canSuspend = false;
            // "error" leaves the optimistic default untouched.
        }
    }

    Component.onCompleted: canSuspendProc.request("power-can-suspend")

    onVisibleChanged: {
        if (visible) {
            root.confirmAction = "";
            canSuspendProc.request("power-can-suspend");
        }
    }

    function focusFirst() {
        suspendScope.forceActiveFocus();
    }

    ColumnLayout {
        anchors.fill: parent
        anchors.margins: Theme.padding
        spacing: 48

        Text {
            text: "Power"
            font.pixelSize: Theme.fontBody
            font.bold: true
            color: Theme.textPrimary
        }

        Item {
            Layout.fillHeight: true
            Layout.maximumHeight: 100
        }

        // Power buttons - large and centered
        ColumnLayout {
            Layout.alignment: Qt.AlignHCenter
            spacing: 32

            FocusScope {
                id: suspendScope
                Layout.preferredWidth: 500
                Layout.preferredHeight: 120
                focus: true
                activeFocusOnTab: true

                KeyNavigation.down: restartScope

                Rectangle {
                    anchors.fill: parent
                    radius: 24
                    color: parent.activeFocus ? Theme.gold : Theme.surface
                    border.width: parent.activeFocus ? 0 : 2
                    border.color: Theme.surfaceHover

                    Behavior on color {
                        ColorAnimation {
                            duration: 150
                        }
                    }

                    ColumnLayout {
                        anchors.centerIn: parent
                        spacing: 4

                        Text {
                            text: "Sleep"
                            font.pixelSize: Theme.fontTitle
                            font.bold: true
                            color: suspendScope.activeFocus ? Theme.textOnDark : Theme.textPrimary
                            Layout.alignment: Qt.AlignHCenter
                        }

                        Text {
                            text: root.canSuspend ? "Suspend to RAM" : "Suspend unavailable"
                            font.pixelSize: Theme.fontSmall
                            color: suspendScope.activeFocus ? Theme.textOnDarkMuted : Theme.textSecondary
                            Layout.alignment: Qt.AlignHCenter
                        }
                    }

                    MouseArea {
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: {
                            suspendScope.forceActiveFocus();
                            if (root.canSuspend)
                                root.confirmAction = "suspend";
                        }
                    }
                }

                Keys.onReturnPressed: {
                    if (root.canSuspend)
                        root.confirmAction = "suspend";
                }
            }

            FocusScope {
                id: restartScope
                Layout.preferredWidth: 500
                Layout.preferredHeight: 120
                activeFocusOnTab: true

                KeyNavigation.up: suspendScope
                KeyNavigation.down: shutdownScope

                Rectangle {
                    anchors.fill: parent
                    radius: 24
                    color: parent.activeFocus ? Theme.ember : Theme.surface
                    border.width: parent.activeFocus ? 0 : 2
                    border.color: Theme.surfaceHover

                    Behavior on color {
                        ColorAnimation {
                            duration: 150
                        }
                    }

                    ColumnLayout {
                        anchors.centerIn: parent
                        spacing: 4

                        Text {
                            text: "Restart"
                            font.pixelSize: Theme.fontTitle
                            font.bold: true
                            color: restartScope.activeFocus ? Theme.textOnDark : Theme.textPrimary
                            Layout.alignment: Qt.AlignHCenter
                        }

                        Text {
                            text: "Reboot the system"
                            font.pixelSize: Theme.fontSmall
                            color: restartScope.activeFocus ? Theme.textOnDarkMuted : Theme.textSecondary
                            Layout.alignment: Qt.AlignHCenter
                        }
                    }

                    MouseArea {
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: {
                            restartScope.forceActiveFocus();
                            root.confirmAction = "restart";
                        }
                    }
                }

                Keys.onReturnPressed: {
                    root.confirmAction = "restart";
                }
            }

            FocusScope {
                id: shutdownScope
                Layout.preferredWidth: 500
                Layout.preferredHeight: 120
                activeFocusOnTab: true

                KeyNavigation.up: restartScope

                Rectangle {
                    anchors.fill: parent
                    radius: 24
                    color: parent.activeFocus ? Theme.crimson : Theme.surface
                    border.width: parent.activeFocus ? 0 : 2
                    border.color: Theme.surfaceHover

                    Behavior on color {
                        ColorAnimation {
                            duration: 150
                        }
                    }

                    ColumnLayout {
                        anchors.centerIn: parent
                        spacing: 4

                        Text {
                            text: "Shutdown"
                            font.pixelSize: Theme.fontTitle
                            font.bold: true
                            color: shutdownScope.activeFocus ? Theme.textOnDark : Theme.textPrimary
                            Layout.alignment: Qt.AlignHCenter
                        }

                        Text {
                            text: "Power off the system"
                            font.pixelSize: Theme.fontSmall
                            color: shutdownScope.activeFocus ? Theme.textOnDarkMuted : Theme.textSecondary
                            Layout.alignment: Qt.AlignHCenter
                        }
                    }

                    MouseArea {
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: {
                            shutdownScope.forceActiveFocus();
                            root.confirmAction = "shutdown";
                        }
                    }
                }

                Keys.onReturnPressed: {
                    root.confirmAction = "shutdown";
                }
            }
        }

        Item {
            Layout.fillHeight: true
        }

        Text {
            text: "A: Select  |  Use with caution"
            font.pixelSize: Theme.fontHint
            color: Theme.textSecondary
            Layout.alignment: Qt.AlignHCenter
        }
    }

    // Confirmation dialog
    Rectangle {
        anchors.fill: parent
        color: Qt.rgba(0, 0, 0, 0.7)
        visible: root.confirmAction !== ""

        MouseArea {
            anchors.fill: parent
            onClicked: {
                root.confirmAction = "";
            }
        }

        Rectangle {
            anchors.centerIn: parent
            width: 700
            height: 350
            radius: 32
            color: Theme.surface

            ColumnLayout {
                anchors.centerIn: parent
                spacing: 32

                Text {
                    text: {
                        switch (root.confirmAction) {
                        case "suspend":
                            return "Sleep this system?";
                        case "restart":
                            return "Restart this system?";
                        case "shutdown":
                            return "Shut down this system?";
                        default:
                            return "";
                        }
                    }
                    font.pixelSize: Theme.fontTitle
                    font.bold: true
                    color: Theme.textPrimary
                    Layout.alignment: Qt.AlignHCenter
                }

                RowLayout {
                    Layout.alignment: Qt.AlignHCenter
                    spacing: 32

                    FocusScope {
                        id: confirmYesScope
                        Layout.preferredWidth: confirmYesBtn.implicitWidth
                        Layout.preferredHeight: confirmYesBtn.implicitHeight
                        activeFocusOnTab: true

                        KeyNavigation.right: confirmNoScope

                        SettingsButton {
                            id: confirmYesBtn
                            text: "Yes"
                            focus: parent.activeFocus
                            anchors.fill: parent

                            MouseArea {
                                anchors.fill: parent
                                hoverEnabled: true
                                cursorShape: Qt.PointingHandCursor
                                onClicked: {
                                    confirmYesScope.forceActiveFocus();
                                    executeAction();
                                }
                            }
                        }

                        Keys.onReturnPressed: executeAction()
                    }

                    FocusScope {
                        id: confirmNoScope
                        Layout.preferredWidth: confirmNoBtn.implicitWidth
                        Layout.preferredHeight: confirmNoBtn.implicitHeight
                        focus: root.confirmAction !== ""
                        activeFocusOnTab: true

                        KeyNavigation.left: confirmYesScope

                        SettingsButton {
                            id: confirmNoBtn
                            text: "Cancel"
                            focus: parent.activeFocus
                            anchors.fill: parent

                            MouseArea {
                                anchors.fill: parent
                                hoverEnabled: true
                                cursorShape: Qt.PointingHandCursor
                                onClicked: {
                                    confirmNoScope.forceActiveFocus();
                                    root.confirmAction = "";
                                }
                            }
                        }

                        Keys.onReturnPressed: {
                            root.confirmAction = "";
                        }
                        Keys.onEscapePressed: {
                            root.confirmAction = "";
                        }
                    }
                }
            }
        }

        Keys.onEscapePressed: {
            root.confirmAction = "";
        }
    }

    function executeAction() {
        switch (root.confirmAction) {
        case "suspend":
            suspendCmd.request("power-suspend");
            break;
        case "restart":
            rebootCmd.running = true;
            break;
        case "shutdown":
            powerOff.running = true;
            break;
        }
        root.confirmAction = "";
    }
}
