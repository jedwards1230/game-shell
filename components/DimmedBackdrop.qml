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
        hoverEnabled: true
        onClicked: root.clicked()
    }

    Text {
        anchors.centerIn: parent
        visible: root.message !== ""
        text: root.message
        textFormat: Text.PlainText
        horizontalAlignment: Text.AlignHCenter
        font.pixelSize: Theme.fontTitle
        color: Theme.textOnDark
    }
}
