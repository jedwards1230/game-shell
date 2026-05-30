import QtQuick

FocusScope {
    id: root

    property Item previousRow: null
    property Item nextRow: null
    property alias model: listView.model
    property alias delegate: listView.delegate
    property alias currentItem: listView.currentItem
    property alias currentIndex: listView.currentIndex
    property alias count: listView.count
    property bool keyNavigationWraps: false

    signal activated
    signal escaped
    signal contextRequested

    readonly property alias listView: listView

    ListView {
        id: listView
        anchors.fill: parent
        anchors.topMargin: -16
        anchors.bottomMargin: -16
        leftMargin: 16
        orientation: ListView.Horizontal
        spacing: Theme.cardSpacing
        clip: false
        highlightMoveDuration: 150
        highlightMoveVelocity: -1
        keyNavigationEnabled: true
        keyNavigationWraps: root.keyNavigationWraps
        focus: true

        // Any nav/activation key means the user is driving with the controller
        // or keyboard, not the mouse — flip out of mouse-mode (no daemon hop).
        // Left/Right are consumed by keyNavigationEnabled, so intercept them
        // explicitly: exit mouse-mode, then advance honoring keyNavigationWraps
        // (the always-wrapping ListView increment/decrement methods would break
        // the default no-wrap behavior).
        Keys.onLeftPressed: {
            Theme.exitMouseMode();
            if (listView.currentIndex > 0)
                listView.currentIndex--;
            else if (root.keyNavigationWraps && listView.count > 0)
                listView.currentIndex = listView.count - 1;
        }
        Keys.onRightPressed: {
            Theme.exitMouseMode();
            if (listView.currentIndex < listView.count - 1)
                listView.currentIndex++;
            else if (root.keyNavigationWraps && listView.count > 0)
                listView.currentIndex = 0;
        }
        Keys.onReturnPressed: {
            Theme.exitMouseMode();
            if (listView.currentItem)
                root.activated();
        }
        Keys.onEnterPressed: {
            Theme.exitMouseMode();
            if (listView.currentItem)
                root.activated();
        }
        Keys.onEscapePressed: {
            Theme.exitMouseMode();
            root.escaped();
        }
        Keys.onTabPressed: event => {
            Theme.exitMouseMode();
            if (listView.currentItem) {
                root.contextRequested();
                event.accepted = true;
            }
        }
        Keys.onUpPressed: {
            Theme.exitMouseMode();
            root._navigateUp();
        }
        Keys.onDownPressed: {
            Theme.exitMouseMode();
            root._navigateDown();
        }
    }

    function _navigateUp() {
        var target = previousRow;
        while (target) {
            if (target.visible) {
                target.forceActiveFocus();
                return;
            }
            target = (target.previousRow !== undefined) ? target.previousRow : null;
        }
    }

    function _navigateDown() {
        var target = nextRow;
        while (target) {
            if (target.visible) {
                target.forceActiveFocus();
                return;
            }
            target = (target.nextRow !== undefined) ? target.nextRow : null;
        }
    }
}
