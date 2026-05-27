import QtQuick
import QtQuick.Layouts
import Quickshell.Io

FocusScope {
    id: root

    property var monitors: []
    property int selectedMonitor: 0

    // --- Processes ---

    Process {
        id: getMonitors
        command: ["hyprctl", "monitors", "-j"]
        stdout: SplitParser {
            property string buffer: ""
            onRead: line => {
                buffer += line;
            }
        }
        onExited: {
            try {
                let data = JSON.parse(getMonitors.stdout.buffer);
                let mons = [];
                for (let i = 0; i < data.length; i++) {
                    let m = data[i];
                    mons.push({
                        name: m.name || "Unknown",
                        description: m.description || "",
                        width: m.width || 0,
                        height: m.height || 0,
                        refreshRate: m.refreshRate || 0,
                        scale: m.scale || 1.0,
                        x: m.x || 0,
                        y: m.y || 0,
                        activeWorkspace: m.activeWorkspace ? m.activeWorkspace.name : "",
                        dpmsStatus: m.dpmsStatus !== false,
                        vrr: m.vrr || false,
                        availableModes: m.availableModes || []
                    });
                }
                root.monitors = mons;
            } catch (e) {
                console.log("Failed to parse monitor data:", e);
            }
            getMonitors.stdout.buffer = "";
        }
    }

    Process {
        id: setScale
        property string monName: ""
        property real scaleVal: 1.0
        command: ["hyprctl", "keyword", "monitor", monName + ",preferred,auto," + scaleVal]
        onExited: {
            getMonitors.running = true;
        }
    }

    Process {
        id: setMode
        property string monName: ""
        property string mode: ""
        command: ["hyprctl", "keyword", "monitor", monName + "," + mode + ",auto,1"]
        onExited: {
            getMonitors.running = true;
        }
    }

    Component.onCompleted: {
        getMonitors.running = true;
    }

    onVisibleChanged: {
        if (visible) {
            getMonitors.running = true;
            monitorList.forceActiveFocus();
        }
    }

    ColumnLayout {
        anchors.fill: parent
        anchors.margins: Theme.padding
        spacing: 32

        Text {
            text: "Displays"
            font.pixelSize: Theme.fontBody
            font.bold: true
            color: Theme.textPrimary
        }

        // Monitor list
        ListView {
            id: monitorList
            Layout.fillWidth: true
            Layout.preferredHeight: Math.min(root.monitors.length * 200, 400)
            spacing: 16
            clip: true
            model: root.monitors
            focus: true

            KeyNavigation.down: scaleRow

            delegate: Rectangle {
                required property int index
                required property var modelData
                width: monitorList.width
                height: 180
                radius: 16
                color: monitorList.currentIndex === index && monitorList.activeFocus ? Theme.surfaceHover : Theme.surface
                border.width: 2
                border.color: Theme.surfaceBorder

                Behavior on color {
                    ColorAnimation {
                        duration: 150
                    }
                }

                property bool isSel: monitorList.currentIndex === index && monitorList.activeFocus

                ColumnLayout {
                    anchors.fill: parent
                    anchors.leftMargin: 24
                    anchors.rightMargin: 24
                    anchors.topMargin: 20
                    anchors.bottomMargin: 20
                    spacing: 8

                    RowLayout {
                        Layout.fillWidth: true
                        spacing: 16

                        Text {
                            text: modelData.name
                            font.pixelSize: Theme.fontBody
                            font.bold: true
                            color: Theme.textPrimary
                        }

                        Text {
                            text: modelData.description
                            font.pixelSize: Theme.fontSmall
                            color: Theme.textSecondary
                            elide: Text.ElideRight
                            Layout.fillWidth: true
                        }
                    }

                    RowLayout {
                        spacing: 32

                        Text {
                            text: "Resolution: " + modelData.width + "x" + modelData.height
                            font.pixelSize: Theme.fontSmall
                            color: Theme.textSecondary
                        }

                        Text {
                            text: "Refresh: " + modelData.refreshRate.toFixed(1) + " Hz"
                            font.pixelSize: Theme.fontSmall
                            color: Theme.textSecondary
                        }

                        Text {
                            text: "Scale: " + modelData.scale.toFixed(1) + "x"
                            font.pixelSize: Theme.fontSmall
                            color: Theme.textSecondary
                        }
                    }

                    RowLayout {
                        spacing: 32

                        Text {
                            text: "Position: " + modelData.x + "," + modelData.y
                            font.pixelSize: Theme.fontSmall
                            color: Theme.textSecondary
                        }

                        Text {
                            text: "DPMS: " + (modelData.dpmsStatus ? "On" : "Off")
                            font.pixelSize: Theme.fontSmall
                            color: Theme.textSecondary
                        }

                        Text {
                            text: "VRR: " + (modelData.vrr ? "On" : "Off")
                            font.pixelSize: Theme.fontSmall
                            color: Theme.textSecondary
                        }
                    }
                }

                MouseArea {
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: {
                        monitorList.currentIndex = index;
                        monitorList.forceActiveFocus();
                        root.selectedMonitor = index;
                    }
                }
            }

            Keys.onReturnPressed: {
                root.selectedMonitor = currentIndex;
                scaleRow.forceActiveFocus();
            }
        }

        // Scale controls
        Text {
            text: "Scale"
            font.pixelSize: Theme.fontBody
            font.bold: true
            color: Theme.textPrimary
            visible: root.monitors.length > 0
        }

        FocusScope {
            id: scaleRow
            Layout.fillWidth: true
            Layout.preferredHeight: 64
            visible: root.monitors.length > 0

            KeyNavigation.up: monitorList
            KeyNavigation.down: modeDropdownScope

            property var scales: [0.5, 1.0, 1.25, 1.5, 1.75, 2.0]
            property int selectedScale: {
                if (root.monitors.length <= root.selectedMonitor)
                    return 1;
                let s = root.monitors[root.selectedMonitor].scale;
                for (let i = 0; i < scales.length; i++) {
                    if (Math.abs(scales[i] - s) < 0.05)
                        return i;
                }
                return 1;
            }
            property int focusedIndex: selectedScale

            Keys.onLeftPressed: {
                if (focusedIndex > 0)
                    focusedIndex--;
            }
            Keys.onRightPressed: {
                if (focusedIndex < scales.length - 1)
                    focusedIndex++;
            }
            Keys.onReturnPressed: {
                if (root.monitors.length > root.selectedMonitor) {
                    setScale.monName = root.monitors[root.selectedMonitor].name;
                    setScale.scaleVal = scales[focusedIndex];
                    setScale.running = true;
                }
            }

            RowLayout {
                anchors.fill: parent
                spacing: 16

                Repeater {
                    model: scaleRow.scales

                    FocusScope {
                        id: scaleScope
                        required property var modelData
                        required property int index
                        width: scaleBtn.width
                        height: scaleBtn.height

                        SettingsButton {
                            id: scaleBtn
                            text: parent.modelData + "x"
                            anchors.fill: parent

                            property bool isCurrent: root.monitors.length > root.selectedMonitor && Math.abs(root.monitors[root.selectedMonitor].scale - parent.modelData) < 0.05
                            property bool isFocused: scaleRow.activeFocus && scaleRow.focusedIndex === scaleScope.index

                            color: isCurrent ? Theme.sidebarActive : isFocused ? Theme.surfaceHover : Theme.surface
                            border.width: isFocused ? 2 : 1
                            border.color: isFocused ? Theme.focusBorder : Theme.surfaceBorder

                            MouseArea {
                                anchors.fill: parent
                                hoverEnabled: true
                                cursorShape: Qt.PointingHandCursor
                                onClicked: {
                                    scaleRow.forceActiveFocus();
                                    scaleRow.focusedIndex = scaleScope.index;
                                    if (root.monitors.length > root.selectedMonitor) {
                                        setScale.monName = root.monitors[root.selectedMonitor].name;
                                        setScale.scaleVal = scaleScope.modelData;
                                        setScale.running = true;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Resolution / Mode dropdown
        Text {
            text: "Resolution"
            font.pixelSize: Theme.fontBody
            font.bold: true
            color: Theme.textPrimary
            visible: root.monitors.length > 0
        }

        FocusScope {
            id: modeDropdownScope
            Layout.fillWidth: true
            Layout.preferredHeight: modeDropdownOpen ? Math.min(modeDropdownList.count * 72 + 80, 600) : 80
            visible: root.monitors.length > 0
            focus: false

            property bool modeDropdownOpen: false
            property var modes: root.monitors.length > root.selectedMonitor ? root.monitors[root.selectedMonitor].availableModes : []
            property string currentMode: {
                if (root.monitors.length <= root.selectedMonitor)
                    return "";
                let mon = root.monitors[root.selectedMonitor];
                return mon.width + "x" + mon.height + "@" + mon.refreshRate.toFixed(6);
            }

            function formatMode(mode) {
                let parts = mode.split("@");
                let res = parts[0] || mode;
                let hz = parts.length > 1 ? parseFloat(parts[1]).toFixed(2) + " Hz" : "";
                return res + (hz ? "  @  " + hz : "");
            }

            KeyNavigation.up: scaleRow

            Behavior on Layout.preferredHeight {
                NumberAnimation {
                    duration: 200
                    easing.type: Easing.OutCubic
                }
            }

            Rectangle {
                id: modeDropdownHeader
                width: parent.width
                height: 80
                radius: 16
                color: modeDropdownScope.activeFocus && !modeDropdownScope.modeDropdownOpen ? Theme.surfaceHover : Theme.surface
                border.width: 2
                border.color: Theme.surfaceBorder

                Behavior on color {
                    ColorAnimation {
                        duration: 150
                    }
                }

                RowLayout {
                    anchors.fill: parent
                    anchors.leftMargin: 24
                    anchors.rightMargin: 24
                    spacing: 16

                    Text {
                        text: modeDropdownScope.formatMode(modeDropdownScope.currentMode)
                        font.pixelSize: Theme.fontSmall
                        color: Theme.textPrimary
                        Layout.fillWidth: true
                    }

                    Text {
                        text: "(current)"
                        font.pixelSize: Theme.fontHint
                        color: Theme.textMuted
                    }

                    Text {
                        text: modeDropdownScope.modeDropdownOpen ? "▲" : "▼"
                        font.pixelSize: Theme.fontSmall
                        color: Theme.textSecondary
                    }
                }

                MouseArea {
                    anchors.fill: parent
                    cursorShape: Qt.PointingHandCursor
                    onClicked: {
                        modeDropdownScope.forceActiveFocus();
                        modeDropdownScope.modeDropdownOpen = !modeDropdownScope.modeDropdownOpen;
                    }
                }
            }

            ListView {
                id: modeDropdownList
                anchors.top: modeDropdownHeader.bottom
                anchors.topMargin: 8
                width: parent.width
                height: parent.height - modeDropdownHeader.height - 8
                spacing: 4
                clip: true
                visible: modeDropdownScope.modeDropdownOpen
                model: modeDropdownScope.modes
                keyNavigationEnabled: true
                highlightFollowsCurrentItem: true
                highlightMoveDuration: 100

                delegate: Rectangle {
                    required property int index
                    required property var modelData
                    width: modeDropdownList.width
                    height: 68
                    radius: 12

                    property bool isCurrent: modelData === modeDropdownScope.currentMode

                    color: {
                        if (isCurrent)
                            return Theme.sidebarActive;
                        if (modeDropdownList.currentIndex === index && modeDropdownList.activeFocus)
                            return Theme.surfaceHover;
                        return Theme.card;
                    }
                    border.width: isCurrent ? 2 : 1
                    border.color: isCurrent ? Theme.focusBorder : Theme.surfaceBorder

                    Behavior on color {
                        ColorAnimation {
                            duration: 150
                        }
                    }

                    Text {
                        anchors.centerIn: parent
                        text: modeDropdownScope.formatMode(modelData) + (isCurrent ? "  (current)" : "")
                        font.pixelSize: Theme.fontSmall
                        color: isCurrent ? Theme.textOnDark : Theme.textPrimary
                    }

                    MouseArea {
                        anchors.fill: parent
                        cursorShape: Qt.PointingHandCursor
                        onClicked: {
                            modeDropdownList.currentIndex = index;
                            modeDropdownList.forceActiveFocus();
                        }
                        onDoubleClicked: {
                            if (root.monitors.length > root.selectedMonitor) {
                                setMode.monName = root.monitors[root.selectedMonitor].name;
                                setMode.mode = modelData;
                                setMode.running = true;
                                modeDropdownScope.modeDropdownOpen = false;
                            }
                        }
                    }
                }

                Keys.onReturnPressed: {
                    if (currentIndex >= 0 && root.monitors.length > root.selectedMonitor) {
                        let modes = modeDropdownScope.modes;
                        if (currentIndex < modes.length) {
                            setMode.monName = root.monitors[root.selectedMonitor].name;
                            setMode.mode = modes[currentIndex];
                            setMode.running = true;
                            modeDropdownScope.modeDropdownOpen = false;
                        }
                    }
                }

                Keys.onEscapePressed: {
                    modeDropdownScope.modeDropdownOpen = false;
                    modeDropdownScope.forceActiveFocus();
                }
            }

            Keys.onReturnPressed: {
                if (!modeDropdownOpen) {
                    modeDropdownOpen = true;
                    modeDropdownList.forceActiveFocus();
                }
            }

            Keys.onEscapePressed: {
                if (modeDropdownOpen) {
                    modeDropdownOpen = false;
                } else {
                    event.accepted = false;
                }
            }
        }

        Item {
            Layout.fillHeight: true
        }

        Text {
            text: "A: Open/apply mode  |  B: Close dropdown"
            font.pixelSize: Theme.fontHint
            color: Theme.textSecondary
            Layout.alignment: Qt.AlignHCenter
        }
    }
}
