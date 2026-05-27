import QtQuick
import QtQuick.Layouts

Item {
    id: root
    width: Theme.cardWidth
    height: Theme.cardHeight

    required property var app
    property bool isFocused: (activeFocus && !Theme.mouseMode) || (mouseArea.containsMouse && Theme.mouseMode)

    signal activated

    Connections {
        target: Theme
        function onMouseModeChanged() {
            if (!Theme.mouseMode && mouseArea.containsMouse) {
                if (root.ListView.view)
                    root.ListView.view.currentIndex = root.ListView.view.indexAt(root.x, root.y);
                root.forceActiveFocus();
            }
        }
    }

    transform: [
        Scale {
            origin.x: root.width / 2
            origin.y: root.height / 2
            xScale: root.isFocused ? 1.05 : 1.0
            yScale: root.isFocused ? 1.05 : 1.0
            Behavior on xScale {
                NumberAnimation {
                    duration: 250
                    easing.type: Easing.OutCubic
                }
            }
            Behavior on yScale {
                NumberAnimation {
                    duration: 250
                    easing.type: Easing.OutCubic
                }
            }
        }
    ]

    z: root.isFocused ? 10 : 0

    Rectangle {
        id: card
        anchors.fill: parent
        radius: Theme.cardRadius
        color: Theme.cardBackground
        border.width: root.isFocused ? 6 : 2
        border.color: root.isFocused ? Theme.focusBorder : Theme.surfaceBorder

        Behavior on border.width {
            NumberAnimation {
                duration: 200
            }
        }
        Behavior on border.color {
            ColorAnimation {
                duration: 200
            }
        }

        MouseArea {
            id: mouseArea
            anchors.fill: parent
            hoverEnabled: true
            cursorShape: Qt.PointingHandCursor
            onClicked: {
                root.forceActiveFocus();
                root.activated();
            }
        }

        ColumnLayout {
            anchors.fill: parent
            anchors.margins: Theme.padding / 2
            spacing: 8

            Item {
                Layout.fillHeight: true
            }

            // App icon (Freedesktop icon theme)
            Image {
                id: iconImage
                source: root.app.icon ? "image://icon/" + root.app.icon : ""
                sourceSize: Qt.size(Units.iconSizeXL, Units.iconSizeXL)
                Layout.preferredWidth: Units.iconSizeXL
                Layout.preferredHeight: Units.iconSizeXL
                Layout.alignment: Qt.AlignHCenter
                fillMode: Image.PreserveAspectFit
                visible: status === Image.Ready
            }

            // Fallback letter initial when icon unavailable
            Text {
                visible: iconImage.status !== Image.Ready
                text: (root.app.name || "?").charAt(0).toUpperCase()
                font.pixelSize: Units.iconSizeLG
                font.bold: true
                color: Theme.textSecondary
                Layout.preferredWidth: Units.iconSizeXL
                Layout.preferredHeight: Units.iconSizeXL
                Layout.alignment: Qt.AlignHCenter
                horizontalAlignment: Text.AlignHCenter
                verticalAlignment: Text.AlignVCenter
            }

            Item {
                Layout.fillHeight: true
            }

            // App name — marquee scrolls when focused and text overflows
            MarqueeText {
                Layout.fillWidth: true
                Layout.preferredHeight: Theme.fontSmall * 1.3
                Layout.alignment: Qt.AlignHCenter
                animate: root.isFocused
                text: root.app.name || "Unknown"
                font.pixelSize: Theme.fontSmall
                font.bold: true
                color: Theme.textPrimary
            }
        }
    }

    Keys.onReturnPressed: root.activated()
    Keys.onEnterPressed: root.activated()
}
