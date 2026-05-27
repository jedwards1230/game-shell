import QtQuick

BaseCard {
    id: root

    required property var app
    label: root.app.name || "Unknown"

    Image {
        id: iconImage
        anchors.fill: parent
        source: root.app.icon ? "image://icon/" + root.app.icon : ""
        sourceSize: Qt.size(Units.iconSizeXL, Units.iconSizeXL)
        fillMode: Image.PreserveAspectFit
        visible: status === Image.Ready
    }

    Text {
        visible: iconImage.status !== Image.Ready
        anchors.fill: parent
        text: (root.app.name || "?").charAt(0).toUpperCase()
        font.pixelSize: Units.iconSizeLG
        font.bold: true
        color: Theme.textSecondary
        horizontalAlignment: Text.AlignHCenter
        verticalAlignment: Text.AlignVCenter
    }
}
