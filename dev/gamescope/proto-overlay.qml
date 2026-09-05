import QtQuick
import QtQuick.Window

// Overlay test client. launch.sh runs it as an X11 window and tags it with
// STEAM_OVERLAY=1 (+ STEAM_INPUT_FOCUS=1) so gamescope composites it ABOVE the
// focused app without changing the base layer, which is the mechanism a v2
// drawer / QAM over a live stream would use. Semi-transparent on purpose: if
// the app underneath is visible through it, the overlay plane works.
Window {
    id: root

    property int keyCount: 0
    property string lastKey: "none yet"

    color: "transparent"
    title: "tv-shell-proto-overlay"
    visibility: Window.FullScreen
    visible: true

    Item {
        id: keyCatcher

        anchors.fill: parent
        focus: true

        Keys.onPressed: function (event) {
            root.keyCount += 1;
            root.lastKey = event.text.length > 0 ? event.text : ("key " + event.key);
            event.accepted = true;
        }
    }

    Rectangle {
        anchors.left: parent.left
        anchors.top: parent.top
        anchors.bottom: parent.bottom
        color: "#cc182028"
        width: parent.width * 0.3

        Column {
            anchors.fill: parent
            anchors.margins: 40
            spacing: 16

            Text {
                color: "#e8eef2"
                font.pixelSize: 40
                font.weight: Font.DemiBold
                text: "overlay test"
            }
            Text {
                color: "#9fb0ba"
                font.pixelSize: 24
                wrapMode: Text.WordWrap
                width: parent.width
                text: "If you can see the app behind this panel, the overlay plane works. " + "If keys below count up while the app keeps running, STEAM_INPUT_FOCUS works."
            }
            Text {
                color: keyCatcher.activeFocus ? "#7fd68a" : "#e0705a"
                font.pixelSize: 28
                text: keyCatcher.activeFocus ? "keyboard focus: YES" : "keyboard focus: NO"
            }
            Text {
                color: "#e8eef2"
                font.pixelSize: 28
                text: "keys: " + root.keyCount + "  last: " + root.lastKey
            }
        }
    }
}
