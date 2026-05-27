import QtQuick
import QtQuick.Layouts
import Quickshell.Io

Drawer {
    id: root
    edge: "left"
    drawerWidth: Units.gridUnit * 18

    property bool overlayMode: false

    signal settingsRequested
    signal homeSelected

    onOpenedChanged: {
        if (opened) {
            navList.currentIndex = 0;
            navFocusTimer.restart();
        }
    }

    Timer {
        id: navFocusTimer
        interval: 50
        onTriggered: navList.forceActiveFocus()
    }

    ColumnLayout {
        anchors.fill: parent
        spacing: 0

        // === Clock + Date Header ===
        Item {
            Layout.fillWidth: true
            Layout.preferredHeight: Units.gridUnit * 5

            ColumnLayout {
                anchors.centerIn: parent
                spacing: 12

                Text {
                    id: drawerClock
                    font.pixelSize: Theme.fontHero * 0.7
                    font.bold: true
                    color: Theme.textPrimary
                    Layout.alignment: Qt.AlignHCenter

                    Timer {
                        interval: 1000
                        running: root.opened
                        repeat: true
                        triggeredOnStart: true
                        onTriggered: {
                            let now = new Date();
                            drawerClock.text = now.toLocaleTimeString(Qt.locale(), "h:mm AP");
                        }
                    }
                }

                Text {
                    id: drawerDate
                    font.pixelSize: Theme.fontBody
                    color: Theme.textSecondary
                    Layout.alignment: Qt.AlignHCenter

                    Timer {
                        interval: 60000
                        running: root.opened
                        repeat: true
                        triggeredOnStart: true
                        onTriggered: {
                            let now = new Date();
                            drawerDate.text = now.toLocaleDateString(Qt.locale(), "dddd, MMMM d");
                        }
                    }
                }
            }
        }

        Rectangle {
            Layout.fillWidth: true
            height: 2
            color: Theme.surfaceBorder
        }

        // === Top Navigation Items ===
        ListView {
            id: navList
            Layout.fillWidth: true
            Layout.preferredHeight: contentHeight
            model: [
                {
                    label: "Home",
                    icon: "\u{1F3E0}",
                    action: "home"
                }
            ]
            focus: true
            interactive: false
            currentIndex: 0

            delegate: Rectangle {
                required property int index
                required property var modelData
                width: navList.width
                height: Math.round(Units.gridUnit * 2.2)
                color: navList.currentIndex === index && navList.activeFocus ? Theme.surfaceHover : "transparent"
                Behavior on color {
                    ColorAnimation {
                        duration: 150
                    }
                }

                Rectangle {
                    anchors.left: parent.left
                    anchors.verticalCenter: parent.verticalCenter
                    width: 4
                    height: parent.height - 16
                    radius: 2
                    color: Theme.focusBorder
                    visible: navList.currentIndex === index && navList.activeFocus
                }

                RowLayout {
                    anchors.fill: parent
                    anchors.leftMargin: Units.gridUnit
                    anchors.rightMargin: Units.gridUnit
                    spacing: Units.spacingLG

                    Text {
                        text: modelData.icon
                        font.pixelSize: Theme.fontTitle
                        Layout.preferredWidth: Units.gridUnit
                        horizontalAlignment: Text.AlignHCenter
                    }
                    Text {
                        text: modelData.label
                        font.pixelSize: Theme.fontTitle
                        color: Theme.textPrimary
                        Layout.fillWidth: true
                    }
                }

                MouseArea {
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: root._activateNav(index)
                }
            }

            Keys.onDownPressed: {
                bottomList.forceActiveFocus();
            }
            Keys.onUpPressed: {
                if (currentIndex > 0)
                    currentIndex--;
            }
            Keys.onReturnPressed: root._activateNav(currentIndex)
        }

        // === Spacer ===
        Item {
            Layout.fillWidth: true
            Layout.fillHeight: true
        }

        // === Bottom Section: Settings ===
        Rectangle {
            Layout.fillWidth: true
            height: 2
            color: Theme.surfaceBorder
        }

        ListView {
            id: bottomList
            Layout.fillWidth: true
            Layout.preferredHeight: contentHeight
            model: [
                {
                    label: "Settings",
                    icon: "⚙",
                    action: "settings"
                }
            ]
            interactive: false
            currentIndex: 0

            delegate: Rectangle {
                required property int index
                required property var modelData
                width: bottomList.width
                height: Math.round(Units.gridUnit * 2.2)
                color: bottomList.currentIndex === index && bottomList.activeFocus ? Theme.surfaceHover : "transparent"
                Behavior on color {
                    ColorAnimation {
                        duration: 150
                    }
                }

                Rectangle {
                    anchors.left: parent.left
                    anchors.verticalCenter: parent.verticalCenter
                    width: 4
                    height: parent.height - 16
                    radius: 2
                    color: Theme.focusBorder
                    visible: bottomList.currentIndex === index && bottomList.activeFocus
                }

                RowLayout {
                    anchors.fill: parent
                    anchors.leftMargin: Units.gridUnit
                    anchors.rightMargin: Units.gridUnit
                    spacing: Units.spacingLG

                    Text {
                        text: modelData.icon
                        font.pixelSize: Theme.fontTitle
                        Layout.preferredWidth: Units.gridUnit
                        horizontalAlignment: Text.AlignHCenter
                    }
                    Text {
                        text: modelData.label
                        font.pixelSize: Theme.fontTitle
                        color: Theme.textPrimary
                        Layout.fillWidth: true
                    }
                }

                MouseArea {
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: root._activateBottom(index)
                }
            }

            Keys.onUpPressed: {
                navList.forceActiveFocus();
            }
            Keys.onDownPressed: {
                if (currentIndex < count - 1)
                    currentIndex++;
            }
            Keys.onReturnPressed: root._activateBottom(currentIndex)
        }
    }

    function _activateNav(index) {
        let items = navList.model;
        if (index < 0 || index >= items.length)
            return;
        switch (items[index].action) {
        case "home":
            root.homeSelected();
            break;
        }
    }

    function _activateBottom(index) {
        let items = bottomList.model;
        if (index < 0 || index >= items.length)
            return;
        switch (items[index].action) {
        case "settings":
            root.closed();
            root.settingsRequested();
            break;
        }
    }
}
