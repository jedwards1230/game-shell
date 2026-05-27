import QtQuick
import QtQuick.Layouts

FocusScope {
    id: root

    property bool opened: false
    property int _selectedIndex: 0

    signal errorLogRequested

    visible: opened
    anchors.fill: parent
    focus: opened

    onOpenedChanged: {
        if (opened) {
            NotificationManager.markAllRead();
            _selectedIndex = 0;
            root.forceActiveFocus();
        }
    }

    Keys.onPressed: event => {
        var entries = NotificationManager.history;

        if (event.key === Qt.Key_Up) {
            if (_selectedIndex > 0)
                _selectedIndex--;
        } else if (event.key === Qt.Key_Down) {
            if (_selectedIndex < entries.length - 1)
                _selectedIndex++;
        } else if (event.key === Qt.Key_Return || event.key === Qt.Key_Enter) {
            if (entries.length > 0) {
                var entry = entries[_selectedIndex];
                if (entry.level === "error")
                    root.errorLogRequested();
            }
        } else if (event.key === Qt.Key_Escape) {
            root.opened = false;
        } else if (event.key === Qt.Key_Delete || event.key === Qt.Key_Backspace) {
            NotificationManager.clearHistory();
            ErrorLog.clear();
            _selectedIndex = 0;
        }
        // Modal: block all keys from reaching items below
        event.accepted = true;
    }

    // Backdrop
    DimmedBackdrop {
        dimLevel: 0.85
        onClicked: root.opened = false
    }

    // Content
    Item {
        anchors.fill: parent
        anchors.margins: Units.gridUnit * 3

        // Title
        Text {
            id: titleText
            anchors.top: parent.top
            anchors.left: parent.left
            text: "Notifications" + (NotificationManager.history.length > 0 ? "  (" + NotificationManager.history.length + ")" : "")
            font.pixelSize: Theme.fontTitle
            font.bold: true
            color: Theme.textPrimary
        }

        // Empty state
        Text {
            anchors.centerIn: parent
            visible: NotificationManager.history.length === 0
            text: "No notifications"
            font.pixelSize: Theme.fontBody
            color: Theme.textMuted
        }

        // Notification list
        ListView {
            id: notifList
            anchors.top: titleText.bottom
            anchors.topMargin: Units.spacingLG
            anchors.left: parent.left
            anchors.right: parent.right
            anchors.bottom: hintBar.top
            anchors.bottomMargin: Units.spacingLG
            clip: true
            model: NotificationManager.history
            currentIndex: root._selectedIndex
            spacing: Units.spacingSM

            delegate: Rectangle {
                id: notifDelegate
                required property var modelData
                required property int index

                width: notifList.width
                height: notifColumn.height + Units.spacingMD * 2
                radius: Units.radiusMD
                color: index === root._selectedIndex ? Theme.surfaceHover : Theme.surface
                border.width: index === root._selectedIndex ? Units.borderMedium : Units.borderThin
                border.color: index === root._selectedIndex ? Theme.crimson : Theme.surfaceBorder

                Column {
                    id: notifColumn
                    anchors.left: parent.left
                    anchors.right: parent.right
                    anchors.top: parent.top
                    anchors.margins: Units.spacingMD
                    spacing: Units.spacingXS

                    Row {
                        width: parent.width
                        spacing: Units.spacingSM

                        // Timestamp
                        Text {
                            text: {
                                var d = modelData.timestamp;
                                var h = d.getHours();
                                var m = d.getMinutes();
                                var s = d.getSeconds();
                                return (h < 10 ? "0" : "") + h + ":" + (m < 10 ? "0" : "") + m + ":" + (s < 10 ? "0" : "") + s;
                            }
                            font.pixelSize: Theme.fontSmall
                            font.family: "monospace"
                            color: Theme.textMuted
                            anchors.verticalCenter: parent.verticalCenter
                        }

                        // Level badge
                        Rectangle {
                            width: levelLabel.implicitWidth + Units.spacingSM * 2
                            height: levelLabel.implicitHeight + Units.spacingXS
                            radius: Units.radiusSM
                            color: {
                                if (modelData.level === "error")
                                    return Theme.crimson;
                                if (modelData.level === "warning")
                                    return Theme.ember;
                                return Theme.navy;
                            }
                            anchors.verticalCenter: parent.verticalCenter

                            Text {
                                id: levelLabel
                                anchors.centerIn: parent
                                text: modelData.level
                                font.pixelSize: Theme.fontCaption
                                font.bold: true
                                color: Theme.textOnDark
                            }
                        }

                        // Title
                        Text {
                            text: modelData.title
                            font.pixelSize: Theme.fontBody
                            font.bold: true
                            color: Theme.textPrimary
                            anchors.verticalCenter: parent.verticalCenter
                        }

                        // Message
                        Text {
                            width: parent.width - x
                            text: modelData.message
                            font.pixelSize: Theme.fontSmall
                            color: Theme.textSecondary
                            elide: Text.ElideRight
                            anchors.verticalCenter: parent.verticalCenter
                        }
                    }

                    // Error detail hint
                    Text {
                        visible: modelData.level === "error" && index === root._selectedIndex
                        text: "A: View Details"
                        font.pixelSize: Theme.fontCaption
                        color: Theme.textMuted
                    }
                }
            }
        }

        // Bottom hint bar
        Rectangle {
            id: hintBar
            anchors.bottom: parent.bottom
            anchors.left: parent.left
            anchors.right: parent.right
            height: hintText.implicitHeight + Units.spacingSM * 2
            radius: Units.radiusMD
            color: Theme.surface

            Text {
                id: hintText
                anchors.centerIn: parent
                text: "D-pad: Navigate    A: Error Details    B: Close    X: Clear All"
                font.pixelSize: Theme.fontHint
                color: Theme.textMuted
            }
        }
    }
}
