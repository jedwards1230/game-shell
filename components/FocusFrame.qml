import QtQuick

Item {
    id: root

    property bool focused: false
    property int focusBorderWidth: 6
    property int restBorderWidth: 2
    property color focusBorderColor: Theme.focusBorder
    property color restBorderColor: Theme.surfaceBorder
    property bool scaleEnabled: true
    property real focusScale: 1.05
    property real restScale: 1.0
    property color backgroundColor: Theme.cardBackground
    property real radius: Theme.cardRadius
    property int scaleDuration: 250
    property int borderDuration: 200
    property int focusZ: 10
    property int restZ: 0

    default property alias content: contentArea.data

    z: root.focused ? root.focusZ : root.restZ

    transform: [
        Scale {
            origin.x: root.width / 2
            origin.y: root.height / 2
            xScale: root.scaleEnabled && root.focused ? root.focusScale : root.restScale
            yScale: root.scaleEnabled && root.focused ? root.focusScale : root.restScale
            Behavior on xScale {
                NumberAnimation {
                    duration: root.scaleDuration
                    easing.type: Easing.OutCubic
                }
            }
            Behavior on yScale {
                NumberAnimation {
                    duration: root.scaleDuration
                    easing.type: Easing.OutCubic
                }
            }
        }
    ]

    Rectangle {
        id: frame
        anchors.fill: parent
        radius: root.radius
        color: root.backgroundColor
        border.width: root.focused ? root.focusBorderWidth : root.restBorderWidth
        border.color: root.focused ? root.focusBorderColor : root.restBorderColor

        Behavior on border.width {
            NumberAnimation {
                duration: root.borderDuration
            }
        }
        Behavior on border.color {
            ColorAnimation {
                duration: root.borderDuration
            }
        }

        Item {
            id: contentArea
            anchors.fill: parent
        }
    }
}
