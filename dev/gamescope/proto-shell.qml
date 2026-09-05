import QtQuick
import QtQuick.Window

// Prototype shell for the gamescope session. Deliberately dumb: it shows what
// the compositor is doing to it (size, focus, keys) and nothing else. Launching
// apps and overlays is done from launch.sh over SSH, because the point of this
// week is to measure gamescope, not to rebuild the shell inside it.
Window {
    id: root

    property int keyCount: 0
    property string lastKey: "none yet"
    property string startedAt: new Date().toLocaleTimeString(Qt.locale(), "HH:mm:ss")

    color: "#0f1418"
    title: "tv-shell-proto"
    visibility: Window.FullScreen
    visible: true

    function keyName(event) {
        switch (event.key) {
        case Qt.Key_Up:
            return "Up";
        case Qt.Key_Down:
            return "Down";
        case Qt.Key_Left:
            return "Left";
        case Qt.Key_Right:
            return "Right";
        case Qt.Key_Return:
        case Qt.Key_Enter:
            return "Enter";
        case Qt.Key_Escape:
            return "Escape";
        case Qt.Key_Backspace:
            return "Backspace";
        case Qt.Key_Space:
            return "Space";
        }
        return event.text.length > 0 ? event.text : ("key " + event.key);
    }

    Item {
        id: keyCatcher

        anchors.fill: parent
        focus: true

        Keys.onPressed: function (event) {
            root.keyCount += 1;
            root.lastKey = root.keyName(event);
            event.accepted = true;
        }
    }

    Rectangle {
        // A hard-edged full-white and full-black strip so the SDR black floor
        // and the SDR white level are visible on the TV under --hdr-enabled.
        anchors.top: parent.top
        anchors.left: parent.left
        anchors.right: parent.right
        height: 48

        gradient: Gradient {
            orientation: Gradient.Horizontal
            GradientStop {
                position: 0.0
                color: "#000000"
            }
            GradientStop {
                position: 1.0
                color: "#ffffff"
            }
        }
    }

    Column {
        anchors.centerIn: parent
        spacing: 18
        width: parent.width * 0.7

        Text {
            color: "#e8eef2"
            font.pixelSize: 56
            font.weight: Font.DemiBold
            text: "tv-shell gamescope prototype"
        }
        Text {
            color: "#9fb0ba"
            font.pixelSize: 28
            text: "window " + root.width + "x" + root.height + "  ·  started " + root.startedAt + "  ·  " + clock.now
        }
        Text {
            color: keyCatcher.activeFocus ? "#7fd68a" : "#e0705a"
            font.pixelSize: 34
            text: keyCatcher.activeFocus ? "keyboard focus: YES" : "keyboard focus: NO"
        }
        Text {
            color: "#e8eef2"
            font.pixelSize: 34
            text: "last key: " + root.lastKey + "   (" + root.keyCount + " received)"
        }
        Text {
            color: "#9fb0ba"
            font.pixelSize: 24
            wrapMode: Text.WordWrap
            width: parent.width
            text: "Press D-pad / buttons on the gamepad: keys arrive here through the tv-shell daemon's uinput keyboard. " + "Launch apps, overlays and focus changes from another machine with dev/gamescope/launch.sh and focus.sh; " + "read the numbers with measure.sh."
        }
    }

    Rectangle {
        // Moving element so a frozen frame is distinguishable from a live one
        // in a screenshot or on the TV.
        anchors.bottom: parent.bottom
        anchors.bottomMargin: 40
        color: "#3aa0d8"
        height: 24
        radius: 12
        width: 24
        x: 40 + (root.width - 104) * beat.phase

        NumberAnimation on opacity {
            duration: 1000
            from: 1.0
            loops: Animation.Infinite
            to: 0.3
        }
    }

    Timer {
        id: clock

        property string now: new Date().toLocaleTimeString(Qt.locale(), "HH:mm:ss")

        interval: 1000
        repeat: true
        running: true

        onTriggered: now = new Date().toLocaleTimeString(Qt.locale(), "HH:mm:ss")
    }

    Timer {
        id: beat

        property real phase: 0.0
        property real direction: 1.0

        interval: 16
        repeat: true
        running: true

        onTriggered: {
            phase += direction * 0.004;
            if (phase >= 1.0 || phase <= 0.0)
                direction = -direction;
        }
    }
}
