import QtQuick
import QtQuick.Layouts

FocusScope {
    id: root

    // Each action: { label, action, secondaryAction?, enabled? }. `action` fires on
    // A/Return (and closes); optional `secondaryAction` fires on the X face and keeps
    // the menu open (e.g. "set default" without dismissing). `enabled: false` renders
    // the row muted and makes A a no-op that KEEPS the menu open — an unavailable
    // action stays visible (and stays selectable, so its `hint` can say why) instead
    // of silently vanishing from the menu.
    property var actions: []
    property bool opened: false

    // Optional footer hint, e.g. "A: Use   X: Set default". Shown when non-empty.

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
        width: Math.round(Units.gridUnit * 5.56)
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
                    height: Math.round(Units.gridUnit * 0.96)
                    radius: 10
                    color: index === root._selectedIndex ? Theme.surfaceHover : "transparent"

                    Behavior on color {
                        ColorAnimation {
                            duration: 100
                        }
                    }

                    // Group separator: a thin line at the top edge marks the start
                    // of a new group (e.g. profiles below the stream controls).
                    Rectangle {
                        visible: modelData.dividerBefore === true
                        anchors.top: parent.top
                        anchors.left: parent.left
                        anchors.right: parent.right
                        anchors.leftMargin: 6
                        anchors.rightMargin: 6
                        height: 1
                        color: Theme.surfaceBorder
                    }

                    Text {
                        anchors.centerIn: parent
                        text: modelData.label
                        font.pixelSize: Theme.fontSmall
                        font.bold: index === root._selectedIndex
                        // Muted label = the disabled affordance (same treatment a
                        // locked SteamCard's title gets).
                        color: modelData.enabled === false ? Theme.textMuted : Theme.textPrimary
                    }

                    MouseArea {
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: modelData.enabled === false ? Qt.ArrowCursor : Qt.PointingHandCursor
                        onEntered: root._selectedIndex = index
                        onClicked: root._activateItem(index)
                    }
                }
            }

            // Footer hint reflects the SELECTED item (per-item `hint`), so e.g.
            // "X: Set default" shows only on profile rows, not on Resume/Quit.
            Text {
                readonly property string _selHint: (root._selectedIndex >= 0 && root._selectedIndex < root.actions.length && root.actions[root._selectedIndex] && root.actions[root._selectedIndex].hint) ? root.actions[root._selectedIndex].hint : ""
                visible: _selHint !== ""
                width: menuColumn.width
                horizontalAlignment: Text.AlignHCenter
                topPadding: Units.spacingSM
                text: _selHint
                font.pixelSize: Theme.fontHint
                color: Theme.textMuted
            }
        }
    }

    function _activateItem(idx) {
        // A disabled item is a deliberate no-op that KEEPS the menu open, so its
        // `hint` (the reason it's unavailable) stays on screen.
        if (idx >= 0 && idx < actions.length && actions[idx] && actions[idx].enabled === false)
            return;
        if (idx >= 0 && idx < actions.length && actions[idx].action)
            actions[idx].action();
        root.closed();
    }

    // Fire an item's optional secondary (X) action WITHOUT closing the menu.
    function _secondaryItem(idx) {
        if (idx >= 0 && idx < actions.length && actions[idx].secondaryAction)
            actions[idx].secondaryAction();
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
    // X face (daemon altAction → KEY_X) = the per-item secondary action.
    // Keyboard `b` = back (the gamepad B face already arrives as Escape, handled
    // above). Without this branch a literal `b` bubbles past the popover to its
    // host — harmless on Home, but inside the nav drawer it hits Drawer's Key_B
    // handler and closes the whole drawer out from under an open menu.
    Keys.onPressed: event => {
        if (event.key === Qt.Key_X) {
            root._secondaryItem(root._selectedIndex);
            event.accepted = true;
        } else if (event.key === Qt.Key_B && !event.modifiers) {
            root.closed();
            event.accepted = true;
        }
    }
}
