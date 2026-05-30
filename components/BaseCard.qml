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
            // Hover flips to mouse-mode only on a GENUINE pointer move, filtered
            // by Theme.pointerMoved (global-coords delta). We deliberately do NOT
            // re-add onEntered: containsMouse flips when a row scrolls a card
            // under a *stationary* cursor, which would hijack controller-nav
            // focus mid-nav (#45). Coords mapped to the scene root (null) — a
            // stable frame that doesn't scroll, so content-scroll yields zero
            // delta. (mapToItem(null,...) over mapToGlobal: mapToGlobal is not
            // used anywhere in this codebase, mapToItem is — easy to swap.)
            onPositionChanged: mouse => {
                let p = mapToItem(null, mouse.x, mouse.y);
                Theme.pointerMoved(p.x, p.y);
            }
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
