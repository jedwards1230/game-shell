import QtQuick

FocusScope {
    id: root

    property bool opened: false

    visible: opened
    z: 60
    anchors.fill: parent
    focus: opened

    property int _selectedIndex: 0
    property int _expandedId: -1

    onOpenedChanged: {
        if (opened) {
            _selectedIndex = 0;
            _expandedId = -1;
            root.forceActiveFocus();
        }
    }

    function _reversedEntries() {
        var list = ErrorLog.entries.slice();
        list.reverse();
        return list;
    }

    Keys.onPressed: event => {
        var entries = _reversedEntries();

        if (event.key === Qt.Key_Up) {
            if (_selectedIndex > 0)
                _selectedIndex--;
        } else if (event.key === Qt.Key_Down) {
            if (_selectedIndex < entries.length - 1)
                _selectedIndex++;
        } else if (event.key === Qt.Key_Return || event.key === Qt.Key_Enter) {
            if (entries.length > 0) {
                var entry = entries[_selectedIndex];
                _expandedId = (_expandedId === entry.id) ? -1 : entry.id;
            }
        } else if (event.key === Qt.Key_Escape) {
            root.opened = false;
        } else if (event.key === Qt.Key_Delete || event.key === Qt.Key_Backspace) {
            ErrorLog.clear();
            _selectedIndex = 0;
            _expandedId = -1;
        }
        // Modal: block all keys from reaching items below
        event.accepted = true;
    }

    // Backdrop
    DimmedBackdrop {
        dimLevel: 0.85
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
            text: "Error Log" + (ErrorLog.count > 0 ? "  (" + ErrorLog.count + ")" : "")
            font.pixelSize: Theme.fontTitle
            font.bold: true
            color: Theme.textPrimary
        }

        // Empty state
        Text {
            anchors.centerIn: parent
            visible: ErrorLog.count === 0
            text: "No errors recorded"
            font.pixelSize: Theme.fontBody
            color: Theme.textMuted
        }

        // Error list
        ListView {
            id: errorList
            anchors.top: titleText.bottom
            anchors.topMargin: Units.spacingLG
            anchors.left: parent.left
            anchors.right: parent.right
            anchors.bottom: hintBar.top
            anchors.bottomMargin: Units.spacingLG
            clip: true
            model: root._reversedEntries()
            currentIndex: root._selectedIndex
            spacing: Units.spacingSM

            delegate: Rectangle {
                id: entryDelegate
                required property var modelData
                required property int index

                width: errorList.width
                height: entryColumn.height + Units.spacingMD * 2
                radius: Units.radiusMD
                color: index === root._selectedIndex ? Theme.surfaceHover : Theme.surface
                border.width: index === root._selectedIndex ? Units.borderMedium : Units.borderThin
                border.color: index === root._selectedIndex ? Theme.crimson : Theme.surfaceBorder

                property bool expanded: root._expandedId === modelData.id

                Column {
                    id: entryColumn
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

                        // Source label
                        Rectangle {
                            width: sourceLabel.implicitWidth + Units.spacingSM * 2
                            height: sourceLabel.implicitHeight + Units.spacingXS
                            radius: Units.radiusSM
                            color: Theme.crimson
                            anchors.verticalCenter: parent.verticalCenter

                            Text {
                                id: sourceLabel
                                anchors.centerIn: parent
                                text: ErrorLog._sourceLabel(modelData.source)
                                font.pixelSize: Theme.fontCaption
                                font.bold: true
                                color: Theme.textOnDark
                            }
                        }

                        // Message
                        Text {
                            width: parent.width - x
                            text: modelData.message
                            font.pixelSize: Theme.fontBody
                            color: Theme.textPrimary
                            elide: Text.ElideRight
                            anchors.verticalCenter: parent.verticalCenter
                        }
                    }

                    // Target info
                    Text {
                        visible: modelData.target !== ""
                        text: "Target: " + modelData.target
                        font.pixelSize: Theme.fontCaption
                        color: Theme.textMuted
                    }

                    // Expanded details
                    Rectangle {
                        visible: entryDelegate.expanded && modelData.details !== ""
                        width: parent.width
                        height: visible ? detailsText.implicitHeight + Units.spacingSM * 2 : 0
                        radius: Units.radiusSM
                        color: Qt.rgba(0, 0, 0, 0.3)

                        Text {
                            id: detailsText
                            anchors.fill: parent
                            anchors.margins: Units.spacingSM
                            text: modelData.details
                            font.pixelSize: Theme.fontSmall
                            font.family: "monospace"
                            color: Theme.textSecondary
                            wrapMode: Text.Wrap
                        }
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
                text: "D-pad: Navigate    A: Details    B: Close    X: Clear All"
                font.pixelSize: Theme.fontHint
                color: Theme.textMuted
            }
        }
    }
}
