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

        Keys.onReturnPressed: {
            if (listView.currentItem)
                root.activated();
        }
        Keys.onEnterPressed: {
            if (listView.currentItem)
                root.activated();
        }
        Keys.onEscapePressed: root.escaped()
        Keys.onTabPressed: event => {
            if (listView.currentItem) {
                root.contextRequested();
                event.accepted = true;
            }
        }
        Keys.onUpPressed: root._navigateUp()
        Keys.onDownPressed: root._navigateDown()
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
