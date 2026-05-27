import QtQuick
import QtQuick.Layouts

Rectangle {
    id: root
    color: Qt.rgba(0, 0, 0, 0.85)
    visible: false

    property string message: ""
    property int attemptCount: 0
    property int maxAttempts: 5

    function show(msg) {
        message = msg;
        visible = true;
    }

    function hide() {
        visible = false;
        attemptCount = 0;
    }

    ColumnLayout {
        anchors.centerIn: parent
        spacing: 16

        Text {
            text: root.message
            font.pixelSize: Theme.fontTitle
            color: Theme.textOnDark
            Layout.alignment: Qt.AlignHCenter
        }

        Text {
            visible: root.attemptCount > 0
            text: "Attempt " + root.attemptCount + " of " + root.maxAttempts
            font.pixelSize: Theme.fontBody
            color: Theme.textOnDarkMuted
            Layout.alignment: Qt.AlignHCenter
        }

        Rectangle {
            visible: root.attemptCount > 0
            Layout.alignment: Qt.AlignHCenter
            width: 200
            height: 4
            radius: 2
            color: Theme.surface

            Rectangle {
                width: parent.width * (root.attemptCount / root.maxAttempts)
                height: parent.height
                radius: 2
                color: Theme.crimson
                Behavior on width {
                    NumberAnimation {
                        duration: 300
                    }
                }
            }
        }
    }
}
