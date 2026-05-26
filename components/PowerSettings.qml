import QtQuick
import QtQuick.Layouts
import Quickshell.Io

FocusScope {
    id: root

    property string confirmAction: ""

    Process { id: powerOff; command: ["systemctl", "poweroff"] }
    Process { id: rebootCmd; command: ["systemctl", "reboot"] }
    Process { id: suspendCmd; command: ["systemctl", "suspend"] }

    onVisibleChanged: {
        if (visible) {
            root.confirmAction = ""
            suspendScope.forceActiveFocus()
        }
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

        Item { Layout.fillHeight: true; Layout.maximumHeight: 100 }

        // Power buttons - large and centered
        ColumnLayout {
            Layout.alignment: Qt.AlignHCenter
            spacing: 32

            FocusScope {
                id: suspendScope
                width: 500
                height: 120
                focus: true
                activeFocusOnTab: true

                KeyNavigation.down: restartScope

                Rectangle {
                    anchors.fill: parent
                    radius: 24
                    color: parent.activeFocus ? Theme.gold : Theme.surface
                    border.width: parent.activeFocus ? 0 : 2
                    border.color: Theme.surfaceHover

                    Behavior on color { ColorAnimation { duration: 150 } }

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
                            text: "Suspend to RAM"
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
                            suspendScope.forceActiveFocus()
                            root.confirmAction = "suspend"
                        }
                    }
                }

                Keys.onReturnPressed: { root.confirmAction = "suspend" }
            }

            FocusScope {
                id: restartScope
                width: 500
                height: 120
                activeFocusOnTab: true

                KeyNavigation.up: suspendScope
                KeyNavigation.down: shutdownScope

                Rectangle {
                    anchors.fill: parent
                    radius: 24
                    color: parent.activeFocus ? Theme.ember : Theme.surface
                    border.width: parent.activeFocus ? 0 : 2
                    border.color: Theme.surfaceHover

                    Behavior on color { ColorAnimation { duration: 150 } }

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
                            restartScope.forceActiveFocus()
                            root.confirmAction = "restart"
                        }
                    }
                }

                Keys.onReturnPressed: { root.confirmAction = "restart" }
            }

            FocusScope {
                id: shutdownScope
                width: 500
                height: 120
                activeFocusOnTab: true

                KeyNavigation.up: restartScope

                Rectangle {
                    anchors.fill: parent
                    radius: 24
                    color: parent.activeFocus ? Theme.crimson : Theme.surface
                    border.width: parent.activeFocus ? 0 : 2
                    border.color: Theme.surfaceHover

                    Behavior on color { ColorAnimation { duration: 150 } }

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
                            shutdownScope.forceActiveFocus()
                            root.confirmAction = "shutdown"
                        }
                    }
                }

                Keys.onReturnPressed: { root.confirmAction = "shutdown" }
            }
        }

        Item { Layout.fillHeight: true }

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
            onClicked: { root.confirmAction = "" }
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
                        switch(root.confirmAction) {
                            case "suspend": return "Sleep this system?"
                            case "restart": return "Restart this system?"
                            case "shutdown": return "Shut down this system?"
                            default: return ""
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
                        width: confirmYesBtn.width
                        height: confirmYesBtn.height
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
                                    confirmYesScope.forceActiveFocus()
                                    executeAction()
                                }
                            }
                        }

                        Keys.onReturnPressed: executeAction()
                    }

                    FocusScope {
                        id: confirmNoScope
                        width: confirmNoBtn.width
                        height: confirmNoBtn.height
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
                                    confirmNoScope.forceActiveFocus()
                                    root.confirmAction = ""
                                }
                            }
                        }

                        Keys.onReturnPressed: { root.confirmAction = "" }
                        Keys.onEscapePressed: { root.confirmAction = "" }
                    }
                }
            }
        }

        Keys.onEscapePressed: { root.confirmAction = "" }
    }

    function executeAction() {
        switch(root.confirmAction) {
            case "suspend": suspendCmd.running = true; break
            case "restart": rebootCmd.running = true; break
            case "shutdown": powerOff.running = true; break
        }
        root.confirmAction = ""
    }
}
