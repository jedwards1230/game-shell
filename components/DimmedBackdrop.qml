import QtQuick

Rectangle {
    id: root
    anchors.fill: parent
    color: Qt.rgba(0, 0, 0, dimLevel)

    property real dimLevel: 0.7
    property string message: ""

    signal clicked

    MouseArea {
        anchors.fill: parent
        onClicked: root.clicked()
    }

    Text {
        anchors.centerIn: parent
        visible: root.message !== ""
        text: root.message
        font.pixelSize: Theme.fontTitle
        color: Theme.textOnDark
    }
}
