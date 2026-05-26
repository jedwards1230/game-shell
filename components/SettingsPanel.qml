import QtQuick
import QtQuick.Layouts

Rectangle {
    id: root
    color: Theme.background
    visible: false

    signal closed()

    property int currentSection: 0
    property var sections: [
        { name: "Audio",       icon: "\u{1F50A}" },
        { name: "Bluetooth",   icon: "⚡" },
        { name: "Network",     icon: "\u{1F310}" },
        { name: "Display",     icon: "\u{1F5A5}" },
        { name: "Controllers", icon: "\u{1F3AE}" },
        { name: "Moonlight",   icon: "\u{1F319}" },
        { name: "Appearance",  icon: "\u{1F3A8}" },
        { name: "Power",       icon: "⏻" }
    ]

    onVisibleChanged: {
        if (visible) {
            currentSection = 0
            sidebarList.currentIndex = 0
            // Delay focus slightly to ensure Loader has settled
            focusTimer.restart()
        }
    }

    Timer {
        id: focusTimer
        interval: 50
        onTriggered: { sidebarList.forceActiveFocus() }
    }

    RowLayout {
        anchors.fill: parent
        spacing: 0

        // Left sidebar
        Rectangle {
            Layout.fillHeight: true
            Layout.preferredWidth: 560
            color: Theme.surface

            ColumnLayout {
                anchors.fill: parent
                spacing: 0

                // Settings title — plain text, no colored bar
                Item {
                    Layout.fillWidth: true
                    height: Theme.statusBarHeight

                    Text {
                        anchors.left: parent.left
                        anchors.leftMargin: 48
                        anchors.verticalCenter: parent.verticalCenter
                        text: "Settings"
                        font.pixelSize: Theme.fontHero * 0.6
                        font.bold: true
                        color: Theme.textPrimary
                    }
                }

                // Section list
                ListView {
                    id: sidebarList
                    Layout.fillWidth: true
                    Layout.fillHeight: true
                    model: root.sections
                    currentIndex: 0
                    focus: true
                    clip: true

                    delegate: Rectangle {
                        required property int index
                        required property var modelData
                        width: sidebarList.width
                        height: 100
                        color: {
                            if (root.currentSection === index)
                                return Theme.sidebarActive
                            if (sidebarList.currentIndex === index && sidebarList.activeFocus)
                                return Theme.surfaceHover
                            return "transparent"
                        }

                        Behavior on color { ColorAnimation { duration: 150 } }

                        // Left accent bar on focused item
                        Rectangle {
                            anchors.left: parent.left
                            anchors.verticalCenter: parent.verticalCenter
                            width: 4
                            height: parent.height - 16
                            radius: 2
                            color: Theme.focusBorder
                            visible: sidebarList.currentIndex === index && sidebarList.activeFocus
                        }

                        RowLayout {
                            anchors.fill: parent
                            anchors.leftMargin: 40
                            anchors.rightMargin: 40
                            spacing: 20

                            Text {
                                text: modelData.icon
                                font.pixelSize: Theme.fontBody
                                Layout.preferredWidth: 64
                                horizontalAlignment: Text.AlignHCenter
                            }

                            Text {
                                text: modelData.name
                                font.pixelSize: Theme.fontBody
                                font.bold: root.currentSection === index
                                color: root.currentSection === index ? Theme.textPrimary : Theme.textSecondary
                                Layout.fillWidth: true
                            }
                        }

                        MouseArea {
                            anchors.fill: parent
                            hoverEnabled: true
                            cursorShape: Qt.PointingHandCursor
                            onClicked: {
                                sidebarList.currentIndex = index
                                root.currentSection = index
                                sidebarList.forceActiveFocus()
                            }
                        }
                    }

                    Keys.onReturnPressed: {
                        root.currentSection = currentIndex
                    }

                    Keys.onRightPressed: {
                        root.currentSection = currentIndex
                        contentLoader.item.forceActiveFocus()
                    }

                    Keys.onUpPressed: {
                        if (currentIndex > 0) currentIndex--
                    }

                    Keys.onDownPressed: {
                        if (currentIndex < root.sections.length - 1) currentIndex++
                    }

                    Keys.onPressed: (event) => {
                        if (event.key === Qt.Key_B && !event.modifiers) {
                            root.closed()
                            event.accepted = true
                        }
                    }
                }

                // Back hint
                Rectangle {
                    Layout.fillWidth: true
                    height: 80
                    color: Theme.surfaceHover

                    Text {
                        anchors.centerIn: parent
                        text: "B: Back to Home"
                        font.pixelSize: Theme.fontHint
                        color: Theme.textSecondary
                    }
                }
            }
        }

        // Divider
        Rectangle {
            Layout.fillHeight: true
            width: 2
            color: Theme.surfaceBorder
        }

        // Right content area
        Item {
            Layout.fillWidth: true
            Layout.fillHeight: true

            // Section title bar
            Rectangle {
                id: sectionHeader
                anchors.top: parent.top
                anchors.left: parent.left
                anchors.right: parent.right
                height: Theme.statusBarHeight
                color: Theme.surfaceHover

                Text {
                    anchors.left: parent.left
                    anchors.leftMargin: 48
                    anchors.verticalCenter: parent.verticalCenter
                    text: root.sections[root.currentSection].icon + "  " + root.sections[root.currentSection].name
                    font.pixelSize: Theme.fontTitle
                    font.bold: true
                    color: Theme.textPrimary
                }
            }

            // Content loader
            Item {
                id: contentArea
                anchors.top: sectionHeader.bottom
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.bottom: parent.bottom

                Loader {
                    id: contentLoader
                    anchors.fill: parent
                    sourceComponent: {
                        switch(root.currentSection) {
                            case 0: return audioComp
                            case 1: return bluetoothComp
                            case 2: return networkComp
                            case 3: return displayComp
                            case 4: return controllerComp
                            case 5: return moonlightComp
                            case 6: return appearanceComp
                            case 7: return powerComp
                            default: return audioComp
                        }
                    }
                }

                Component {
                    id: audioComp
                    AudioSettings {}
                }

                Component {
                    id: bluetoothComp
                    BluetoothSettings {}
                }

                Component {
                    id: networkComp
                    NetworkSettings {}
                }

                Component {
                    id: displayComp
                    DisplaySettings {}
                }

                Component {
                    id: controllerComp
                    ControllerSettings {}
                }

                Component {
                    id: moonlightComp
                    MoonlightSettings {}
                }

                Component {
                    id: appearanceComp
                    AppearanceSettings {}
                }

                Component {
                    id: powerComp
                    PowerSettings {}
                }
            }
        }
    }

    // Global key handling
    Keys.onEscapePressed: root.closed()

    function returnToSidebar() {
        sidebarList.forceActiveFocus()
    }

    Keys.onLeftPressed: {
        if (!sidebarList.activeFocus) {
            returnToSidebar()
        }
    }

    Keys.onPressed: (event) => {
        if (event.key === Qt.Key_B && !event.modifiers) {
            if (!sidebarList.activeFocus) {
                returnToSidebar()
                event.accepted = true
            }
        }
    }
}
