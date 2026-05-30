import QtQuick
import QtQuick.Layouts

Item {
    id: root
    width: Theme.cardWidth
    height: Theme.cardHeight

    property string label: ""
    property bool isFocused: (activeFocus && !Theme.mouseMode) || (mouseArea.containsMouse && Theme.mouseMode)
    default property alias iconContent: iconArea.data

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

    z: root.isFocused ? 10 : 0

    FocusFrame {
        id: focusFrame
        anchors.fill: parent
        focused: root.isFocused

        MouseArea {
            id: mouseArea
            anchors.fill: parent
            hoverEnabled: true
            cursorShape: Qt.PointingHandCursor
            // A real Wayland pointer event flips mouse-mode on directly — the
            // K400 mouse never reaches the daemon, so there is no input-mode
            // event for it (#45). Hover + movement both count as "the mouse is
            // driving" so a stationary-then-wiggle motion is caught.
            onEntered: Theme.enterMouseMode()
            onPositionChanged: Theme.enterMouseMode()
            onClicked: {
                Theme.enterMouseMode();
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

            Item {
                id: iconArea
                Layout.preferredWidth: Units.iconSizeXL
                Layout.preferredHeight: Units.iconSizeXL
                Layout.alignment: Qt.AlignHCenter
            }

            Item {
                Layout.fillHeight: true
            }

            MarqueeText {
                Layout.fillWidth: true
                Layout.preferredHeight: Theme.fontSmall * 1.3
                animate: root.isFocused
                text: root.label
                font.pixelSize: Theme.fontSmall
                font.bold: true
                color: Theme.textPrimary
            }
        }
    }

    Keys.onReturnPressed: root.activated()
    Keys.onEnterPressed: root.activated()
}
