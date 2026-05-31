import QtQuick

Rectangle {
    id: root
    implicitWidth: label.implicitWidth + 80
    implicitHeight: 96
    width: implicitWidth
    height: implicitHeight
    radius: 16
    color: (root.activeFocus && !Theme.mouseMode) || (mouseArea.containsMouse && Theme.mouseMode) ? Theme.surfaceHover : Theme.surface
    border.width: root.activeFocus && !Theme.mouseMode ? 3 : 2
    border.color: root.activeFocus && !Theme.mouseMode ? Theme.focusBorder : Theme.surfaceBorder

    property alias text: label.text

    Connections {
        target: Theme
        function onMouseModeChanged() {
            if (!Theme.mouseMode && mouseArea.containsMouse)
                root.forceActiveFocus();
        }
    }

    Behavior on color {
        ColorAnimation {
            duration: 150
        }
    }
    Behavior on border.color {
        ColorAnimation {
            duration: 150
        }
    }

    MouseArea {
        id: mouseArea
        anchors.fill: parent
        hoverEnabled: true
        cursorShape: Qt.PointingHandCursor
        // Hover flips to mouse-mode only on a GENUINE pointer move, filtered by
        // Theme.pointerMoved (global-coords delta). No onEntered: containsMouse
        // flips when content scrolls under a stationary cursor and would hijack
        // controller-nav focus (#45). Coords mapped to scene root (null) —
        // mapToItem (used elsewhere here) over mapToGlobal (used nowhere).
        onPositionChanged: mouse => {
            let p = mapToItem(null, mouse.x, mouse.y);
            Theme.pointerMoved(p.x, p.y);
        }
        onClicked: {
            Theme.enterMouseMode();
            root.forceActiveFocus();
            root.Keys.returnPressed(null);
        }
    }

    Text {
        id: label
        anchors.centerIn: parent
        font.pixelSize: Theme.fontBody
        color: Theme.textPrimary
    }
}
