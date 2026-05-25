import QtQuick
import QtQuick.Layouts

Item {
    id: root

    property var modes: [
        { id: "auto",  icon: "◐", label: "Auto",  desc: "Follows time of day" },
        { id: "light", icon: "☀", label: "Light", desc: "Light background" },
        { id: "dark",  icon: "☽", label: "Dark",  desc: "OLED optimized" }
    ]

    onVisibleChanged: {
        if (visible) modeList.forceActiveFocus()
    }

    ColumnLayout {
        anchors.fill: parent
        anchors.margins: Theme.padding
        spacing: 48

        Text {
            text: "Theme Mode"
            font.pixelSize: Theme.fontBody
            font.bold: true
            color: Theme.textPrimary
        }

        Item { Layout.fillHeight: true; Layout.maximumHeight: 80 }

        // Mode cards
        RowLayout {
            id: modeList
            Layout.alignment: Qt.AlignHCenter
            spacing: 40
            focus: true

            property int currentIndex: {
                for (var i = 0; i < root.modes.length; i++) {
                    if (root.modes[i].id === Theme.themeMode) return i
                }
                return 0
            }

            property int focusIndex: 0

            Keys.onLeftPressed: {
                if (focusIndex > 0) focusIndex--
            }
            Keys.onRightPressed: {
                if (focusIndex < root.modes.length - 1) focusIndex++
            }
            Keys.onReturnPressed: {
                Theme.setThemeMode(root.modes[focusIndex].id)
            }

            Repeater {
                model: root.modes

                Rectangle {
                    required property var modelData
                    required property int index
                    width: 300
                    height: 200
                    radius: Theme.cardRadius
                    color: Theme.surface
                    border.width: Theme.themeMode === modelData.id ? 3 : (modeList.focusIndex === index && modeList.activeFocus ? 2 : 2)
                    border.color: Theme.themeMode === modelData.id ? Theme.focusBorder :
                                  (modeList.focusIndex === index && modeList.activeFocus ? Theme.focusBorder : Theme.surfaceBorder)

                    Behavior on border.color { ColorAnimation { duration: 150 } }

                    // Focus highlight background
                    Rectangle {
                        anchors.fill: parent
                        radius: parent.radius
                        color: Theme.surfaceHover
                        visible: modeList.focusIndex === index && modeList.activeFocus && Theme.themeMode !== modelData.id
                    }

                    ColumnLayout {
                        anchors.centerIn: parent
                        spacing: 16

                        Text {
                            text: modelData.icon
                            font.pixelSize: Theme.fontHero * 0.6
                            color: Theme.themeMode === modelData.id ? Theme.focusBorder : Theme.textPrimary
                            Layout.alignment: Qt.AlignHCenter
                        }

                        Text {
                            text: modelData.label
                            font.pixelSize: Theme.fontBody
                            font.bold: true
                            color: Theme.textPrimary
                            Layout.alignment: Qt.AlignHCenter
                        }

                        Text {
                            text: modelData.desc
                            font.pixelSize: Theme.fontSmall
                            color: Theme.textSecondary
                            Layout.alignment: Qt.AlignHCenter
                        }
                    }

                    MouseArea {
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: {
                            modeList.focusIndex = index
                            modeList.forceActiveFocus()
                            Theme.setThemeMode(modelData.id)
                        }
                    }
                }
            }
        }

        Item { Layout.fillHeight: true }

        Text {
            text: "Current: " + Theme.themeMode.charAt(0).toUpperCase() + Theme.themeMode.slice(1)
            font.pixelSize: Theme.fontHint
            color: Theme.textSecondary
            Layout.alignment: Qt.AlignHCenter
        }
    }
}
