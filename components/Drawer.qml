import QtQuick

/**
 * Drawer — Generic slide-in drawer from any edge.
 *
 * Usage:
 *   Drawer {
 *       edge: "left"
 *       drawerWidth: 560
 *       opened: someBoolean
 *       onClosed: { someBoolean = false }
 *
 *       Column { ... your content ... }
 *   }
 */
FocusScope {
    id: root

    // === Public API ===
    property string edge: "left"           // "left", "right", "top", "bottom"
    property int drawerWidth: 560          // Width for left/right drawers
    property int drawerHeight: 400         // Height for top/bottom drawers
    property bool opened: false

    // Content goes inside contentContainer
    default property alias content: contentContainer.data

    signal closed()

    // Drawer is always full-parent overlay
    anchors.fill: parent
    visible: opened || _animating
    focus: opened

    // Track whether animation is still running so we stay visible during close
    property bool _animating: false

    // === Scrim (semi-transparent backdrop) ===
    Rectangle {
        id: scrim
        anchors.fill: parent
        color: Qt.rgba(0, 0, 0, 0.5)
        opacity: {
            if (root.edge === "left")
                return 1.0 - Math.abs(drawerPanel.x / root.drawerWidth)
            if (root.edge === "right")
                return 1.0 - Math.abs((parent.width - drawerPanel.x - root.drawerWidth) / root.drawerWidth)
            if (root.edge === "top")
                return 1.0 - Math.abs(drawerPanel.y / root.drawerHeight)
            // bottom
            return 1.0 - Math.abs((parent.height - drawerPanel.y - root.drawerHeight) / root.drawerHeight)
        }

        MouseArea {
            anchors.fill: parent
            onClicked: {
                root.opened = false
                root.closed()
            }
        }
    }

    // === Drawer Panel ===
    Rectangle {
        id: drawerPanel
        color: Theme.surface

        // Size depends on edge
        width: (root.edge === "left" || root.edge === "right") ? root.drawerWidth : parent.width
        height: (root.edge === "top" || root.edge === "bottom") ? root.drawerHeight : parent.height

        // Position: slides in from offscreen
        x: {
            if (root.edge === "left")
                return root.opened ? 0 : -root.drawerWidth
            if (root.edge === "right")
                return root.opened ? parent.width - root.drawerWidth : parent.width
            return 0
        }
        y: {
            if (root.edge === "top")
                return root.opened ? 0 : -root.drawerHeight
            if (root.edge === "bottom")
                return root.opened ? parent.height - root.drawerHeight : parent.height
            return 0
        }

        Behavior on x {
            NumberAnimation {
                duration: 400
                easing.type: Easing.OutCubic
                onRunningChanged: root._animating = running
            }
        }
        Behavior on y {
            NumberAnimation {
                duration: 400
                easing.type: Easing.OutCubic
                onRunningChanged: root._animating = running
            }
        }

        // Content area — caller's children go here
        Item {
            id: contentContainer
            anchors.fill: parent
        }
    }

    // === Key handling: Escape / B closes ===
    Keys.onEscapePressed: {
        root.opened = false
        root.closed()
    }
    Keys.onPressed: (event) => {
        if (event.key === Qt.Key_B && !event.modifiers) {
            root.opened = false
            root.closed()
            event.accepted = true
        }
    }
}
