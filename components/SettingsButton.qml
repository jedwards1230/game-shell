import QtQuick

Rectangle {
    id: root
    implicitWidth: label.implicitWidth + 48
    implicitHeight: 96
    width: implicitWidth
    height: implicitHeight
    radius: 16
    color: root.activeFocus || mouseArea.containsMouse ? Theme.surfaceHover : Theme.surface
    border.width: root.activeFocus ? 3 : 2
    border.color: root.activeFocus ? Theme.focusBorder : Theme.surfaceBorder

    property alias text: label.text

    Behavior on color { ColorAnimation { duration: 150 } }
    Behavior on border.color { ColorAnimation { duration: 150 } }

    MouseArea {
        id: mouseArea
        anchors.fill: parent
        hoverEnabled: true
        cursorShape: Qt.PointingHandCursor
        onClicked: {
            root.forceActiveFocus()
            root.Keys.returnPressed(null)
        }
    }

    Text {
        id: label
        anchors.centerIn: parent
        font.pixelSize: Theme.fontBody
        color: Theme.textPrimary
    }
}
