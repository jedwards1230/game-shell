import QtQuick
import QtQuick.Layouts

FocusScope {
    id: root

    property var actions: []
    property bool opened: false

    property real targetX: 0
    property real targetY: 0

    signal closed

    anchors.fill: parent
    visible: opened
    focus: opened
    z: 20

    property int _selectedIndex: 0

    onOpenedChanged: {
        if (opened)
            _selectedIndex = 0;
    }

    Rectangle {
        anchors.fill: parent
        color: Qt.rgba(0, 0, 0, 0.3)

        MouseArea {
            anchors.fill: parent
            onClicked: root.closed()
        }
    }

    Rectangle {
        id: menuPanel
        width: 300
        height: menuColumn.implicitHeight + 16
        radius: Theme.cardRadius
        color: Theme.surface
        border.width: 2
        border.color: Theme.focusBorder

        x: Math.max(Theme.padding, Math.min(root.targetX - width / 2, root.width - width - Theme.padding))
        y: Math.max(Theme.padding, root.targetY - height - 16)

        Rectangle {
            anchors.fill: parent
            anchors.margins: -4
            radius: parent.radius + 4
            color: Qt.rgba(0, 0, 0, 0.35)
            z: -1
        }

        Column {
            id: menuColumn
            anchors.fill: parent
            anchors.margins: 8

            Repeater {
                model: root.actions

                Rectangle {
                    required property var modelData
                    required property int index
                    width: menuColumn.width
                    height: 52
                    radius: 10
                    color: index === root._selectedIndex ? Theme.surfaceHover : "transparent"

                    Behavior on color {
                        ColorAnimation {
                            duration: 100
                        }
                    }

                    Text {
                        anchors.centerIn: parent
                        text: modelData.label
                        font.pixelSize: Theme.fontSmall
                        font.bold: index === root._selectedIndex
                        color: Theme.textPrimary
                    }

                    MouseArea {
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onEntered: root._selectedIndex = index
                        onClicked: root._activateItem(index)
                    }
                }
            }
        }
    }

    function _activateItem(idx) {
        if (idx >= 0 && idx < actions.length && actions[idx].action)
            actions[idx].action();
        root.closed();
    }

    Keys.onUpPressed: {
        if (_selectedIndex > 0)
            _selectedIndex--;
    }
    Keys.onDownPressed: {
        if (_selectedIndex < actions.length - 1)
            _selectedIndex++;
    }
    Keys.onReturnPressed: _activateItem(_selectedIndex)
    Keys.onEnterPressed: _activateItem(_selectedIndex)
    Keys.onEscapePressed: root.closed()
    Keys.onTabPressed: event => {
        root.closed();
        event.accepted = true;
    }
}
