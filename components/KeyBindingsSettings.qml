import Quickshell.Io
import QtQuick
import QtQuick.Layouts

// IPC protocol: see docs/IPC_PROTOCOL.md
// Commands used: get-bindings, set-binding, capture-next, capture-cancel
// Known issue: hardcodes socket path /run/user/1000/ instead of using GAME_SHELL_SOCK
FocusScope {
    id: root

    property var bindings: [
        {
            action: "select",
            label: "Select / Confirm",
            keys: ["A"],
            category: "Navigation",
            description: "Confirm selection or activate"
        },
        {
            action: "back",
            label: "Back / Cancel",
            keys: ["B"],
            category: "Navigation",
            description: "Go back or cancel"
        },
        {
            action: "altSelect",
            label: "Alt Select",
            keys: ["Y"],
            category: "Navigation",
            description: "Tab navigation"
        },
        {
            action: "confirm",
            label: "Confirm",
            keys: ["Start"],
            category: "Navigation",
            description: "Confirm / enter"
        },
        {
            action: "up",
            label: "Navigate Up",
            keys: ["D-Pad Up"],
            category: "Navigation",
            description: "Move focus up"
        },
        {
            action: "down",
            label: "Navigate Down",
            keys: ["D-Pad Down"],
            category: "Navigation",
            description: "Move focus down"
        },
        {
            action: "left",
            label: "Navigate Left",
            keys: ["D-Pad Left"],
            category: "Navigation",
            description: "Move focus left"
        },
        {
            action: "right",
            label: "Navigate Right",
            keys: ["D-Pad Right"],
            category: "Navigation",
            description: "Move focus right"
        },
        {
            action: "stickUp",
            label: "Stick Up",
            keys: ["Left Stick ↑"],
            category: "Navigation",
            description: "Move focus up"
        },
        {
            action: "stickDown",
            label: "Stick Down",
            keys: ["Left Stick ↓"],
            category: "Navigation",
            description: "Move focus down"
        },
        {
            action: "stickLeft",
            label: "Stick Left",
            keys: ["Left Stick ←"],
            category: "Navigation",
            description: "Move focus left"
        },
        {
            action: "stickRight",
            label: "Stick Right",
            keys: ["Left Stick →"],
            category: "Navigation",
            description: "Move focus right"
        },
        {
            action: "drawer",
            label: "Tap Home",
            keys: ["Home"],
            category: "System",
            description: "Toggle drawer; wakes AV if system is off"
        },
        {
            action: "homeHold",
            label: "Go Home (close app)",
            keys: ["Home (hold 2s)"],
            category: "System",
            description: "Close foreground app and return to home screen"
        },
        {
            action: "mouseLB",
            label: "Left Click",
            keys: ["LB"],
            category: "System",
            description: "Mouse left click (right stick cursor)"
        },
        {
            action: "mouseRB",
            label: "Right Click",
            keys: ["RB"],
            category: "System",
            description: "Mouse right click (right stick cursor)"
        },
        {
            action: "forceQuit",
            label: "Force Quit",
            keys: ["Back", "Home", "LB", "RB"],
            category: "System",
            description: "Instantly kill foreground app"
        },
        {
            action: "endSession",
            label: "End Session",
            keys: ["B", "Home"],
            category: "System",
            description: "Hold 3 seconds to end game session"
        }
    ]

    // Actions that can be remapped via daemon IPC
    property var remappableActions: ["select", "back", "altSelect", "confirm"]

    property var navigationBindings: bindings.filter(function (b) {
        return b.category === "Navigation";
    })
    property var systemBindings: bindings.filter(function (b) {
        return b.category === "System";
    })

    // Capture state
    property int editingIndex: -1
    property bool capturing: false
    property string editingAction: ""
    property string editingLabel: ""

    // Button display name mapping (evdev code name -> friendly name)
    property var buttonDisplayNames: ({
            "BTN_SOUTH": "A",
            "BTN_A": "A",
            "BTN_GAMEPAD": "A",
            "BTN_EAST": "B",
            "BTN_B": "B",
            "BTN_NORTH": "Y",
            "BTN_X": "Y",
            "BTN_WEST": "X",
            "BTN_Y": "X",
            "BTN_TL": "LB",
            "BTN_TR": "RB",
            "BTN_SELECT": "Back",
            "BTN_START": "Start",
            "BTN_MODE": "Home",
            "BTN_THUMBL": "L3",
            "BTN_THUMBR": "R3"
        })

    // Default bindings for reset
    property var defaultBindingMap: ({
            "select": "BTN_SOUTH",
            "back": "BTN_EAST",
            "altSelect": "BTN_NORTH",
            "confirm": "BTN_START",
            "drawer": "BTN_MODE"
        })

    function buttonDisplayName(codeName) {
        return buttonDisplayNames[codeName] || codeName;
    }

    function updateBindingsFromDaemon(daemonBindings) {
        var updated = bindings.slice();
        for (var i = 0; i < updated.length; i++) {
            var action = updated[i].action;
            if (daemonBindings[action] !== undefined) {
                updated[i] = Object.assign({}, updated[i], {
                    keys: [buttonDisplayName(daemonBindings[action])]
                });
            }
        }
        bindings = updated;
    }

    function startCapture(index, action, label) {
        editingIndex = index;
        editingAction = action;
        editingLabel = label;
        capturing = true;
        captureProc.running = true;
        captureOverlay.forceActiveFocus();
    }

    function cancelCapture() {
        capturing = false;
        editingIndex = -1;
        editingAction = "";
        editingLabel = "";
        cancelCaptureProc.running = true;
    }

    function applyBinding(action, buttonName) {
        capturing = false;
        editingIndex = -1;
        setBindingProc.command = ["bash", "-c", "echo 'set-binding " + action + " " + buttonName + "' | socat -t 5 - UNIX-CONNECT:/run/user/1000/game-shell-input.sock"];
        setBindingProc.running = true;
    }

    function resetDefaults() {
        var actions = Object.keys(defaultBindingMap);
        // Build a single command that sends all set-binding commands
        var cmds = "";
        for (var i = 0; i < actions.length; i++) {
            if (i > 0)
                cmds += " && ";
            cmds += "echo 'set-binding " + actions[i] + " " + defaultBindingMap[actions[i]] + "' | socat -t 5 - UNIX-CONNECT:/run/user/1000/game-shell-input.sock";
        }
        resetProc.command = ["bash", "-c", cmds];
        resetProc.running = true;
    }

    // --- IPC Processes ---

    Process {
        id: getBindingsProc
        command: ["bash", "-c", "echo 'get-bindings' | socat -t 5 - UNIX-CONNECT:/run/user/1000/game-shell-input.sock"]
        stdout: SplitParser {
            onRead: line => {
                try {
                    var daemonBindings = JSON.parse(line);
                    root.updateBindingsFromDaemon(daemonBindings);
                } catch (e) {
                    console.log("KeyBindings: failed to parse bindings:", e);
                }
            }
        }
    }

    Process {
        id: captureProc
        command: ["bash", "-c", "echo 'capture-next' | socat -t 15 - UNIX-CONNECT:/run/user/1000/game-shell-input.sock"]
        stdout: SplitParser {
            onRead: line => {
                if (line.startsWith("captured:") && root.capturing) {
                    var buttonName = line.substring(9);
                    root.applyBinding(root.editingAction, buttonName);
                } else if (line === "timeout" || line === "cancelled") {
                    root.cancelCapture();
                }
            }
        }
        onExited: {
            root.capturing = false;
        }
    }

    Process {
        id: cancelCaptureProc
        command: ["bash", "-c", "echo 'capture-cancel' | socat -t 5 - UNIX-CONNECT:/run/user/1000/game-shell-input.sock"]
    }

    Process {
        id: setBindingProc
        // command set dynamically in applyBinding()
        onExited: {
            getBindingsProc.running = true;
        }
    }

    Process {
        id: resetProc
        // command set dynamically in resetDefaults()
        onExited: {
            getBindingsProc.running = true;
        }
    }

    Component.onCompleted: {
        getBindingsProc.running = true;
    }

    onVisibleChanged: {
        if (visible) {
            getBindingsProc.running = true;
            bindingsList.forceActiveFocus();
        }
    }

    ColumnLayout {
        anchors.fill: parent
        anchors.margins: Theme.padding
        spacing: 32

        // Navigation section header
        Text {
            text: "Navigation"
            font.pixelSize: Theme.fontBody
            font.bold: true
            color: Theme.textPrimary
        }

        // Navigation bindings list
        ListView {
            id: bindingsList
            Layout.fillWidth: true
            Layout.preferredHeight: Math.min(root.navigationBindings.length * 88, 1060)
            spacing: 8
            clip: true
            model: root.navigationBindings
            focus: true
            interactive: false
            keyNavigationEnabled: true

            KeyNavigation.down: systemList

            Keys.onReturnPressed: {
                var binding = root.navigationBindings[currentIndex];
                if (root.remappableActions.indexOf(binding.action) >= 0) {
                    root.startCapture(currentIndex, binding.action, binding.label);
                }
            }

            delegate: Rectangle {
                required property int index
                required property var modelData
                width: bindingsList.width
                height: 80
                radius: 16
                color: bindingsList.currentIndex === index && bindingsList.activeFocus ? Theme.surfaceHover : Theme.surface
                border.width: 2
                border.color: {
                    if (root.remappableActions.indexOf(modelData.action) >= 0 && bindingsList.currentIndex === index && bindingsList.activeFocus)
                        return Theme.focusBorder;
                    return Theme.surfaceBorder;
                }

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

                    // Action label
                    Text {
                        text: modelData.label
                        font.pixelSize: Theme.fontSmall
                        color: Theme.textPrimary
                        Layout.fillWidth: true
                    }

                    // Key cap badges
                    Row {
                        Layout.alignment: Qt.AlignRight
                        spacing: 8

                        Repeater {
                            model: modelData.keys

                            Row {
                                spacing: 8

                                // "+" separator for combo keys (after first key)
                                Text {
                                    text: "+"
                                    font.pixelSize: Theme.fontSmall
                                    color: Theme.textMuted
                                    visible: index > 0
                                    anchors.verticalCenter: parent.verticalCenter
                                }

                                Rectangle {
                                    width: keyText.implicitWidth + 24
                                    height: keyText.implicitHeight + 16
                                    radius: 8
                                    color: Theme.surface
                                    border.width: 2
                                    border.color: Theme.ember

                                    Text {
                                        id: keyText
                                        anchors.centerIn: parent
                                        text: modelData
                                        font.pixelSize: Theme.fontSmall
                                        color: Theme.textPrimary
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // System section header
        Text {
            text: "System"
            font.pixelSize: Theme.fontBody
            font.bold: true
            color: Theme.textPrimary
        }

        // System bindings list (read-only)
        ListView {
            id: systemList
            Layout.fillWidth: true
            Layout.preferredHeight: Math.min(root.systemBindings.length * 108, 400)
            spacing: 8
            clip: true
            model: root.systemBindings
            interactive: false
            keyNavigationEnabled: true

            KeyNavigation.up: bindingsList
            KeyNavigation.down: resetButton

            delegate: Rectangle {
                required property int index
                required property var modelData
                width: systemList.width
                height: modelData.action === "drawer" ? 100 : 80
                radius: 16
                color: systemList.currentIndex === index && systemList.activeFocus ? Theme.surfaceHover : Theme.surface
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

                    // Action label + optional subtitle
                    ColumnLayout {
                        Layout.fillWidth: true
                        spacing: 4

                        Text {
                            text: modelData.label
                            font.pixelSize: Theme.fontSmall
                            color: Theme.textPrimary
                        }

                        Text {
                            text: "Also wakes AV system from standby"
                            font.pixelSize: Theme.fontHint
                            color: Theme.textMuted
                            visible: modelData.action === "drawer"
                        }
                    }

                    // Key cap badges
                    Row {
                        Layout.alignment: Qt.AlignRight
                        spacing: 8

                        Repeater {
                            model: modelData.keys

                            Row {
                                spacing: 8

                                Text {
                                    text: "+"
                                    font.pixelSize: Theme.fontSmall
                                    color: Theme.textMuted
                                    visible: index > 0
                                    anchors.verticalCenter: parent.verticalCenter
                                }

                                Rectangle {
                                    width: sysKeyText.implicitWidth + 24
                                    height: sysKeyText.implicitHeight + 16
                                    radius: 8
                                    color: Theme.surface
                                    border.width: 2
                                    border.color: Theme.ember

                                    Text {
                                        id: sysKeyText
                                        anchors.centerIn: parent
                                        text: modelData
                                        font.pixelSize: Theme.fontSmall
                                        color: Theme.textPrimary
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Reset to Defaults button
        SettingsButton {
            id: resetButton
            text: "Reset to Defaults"
            Layout.alignment: Qt.AlignHCenter

            KeyNavigation.up: systemList

            Keys.onReturnPressed: {
                root.resetDefaults();
            }
        }

        Item {
            Layout.fillHeight: true
        }

        Text {
            text: "A: Edit binding  |  B: Back"
            font.pixelSize: Theme.fontHint
            color: Theme.textSecondary
            Layout.alignment: Qt.AlignHCenter
        }
    }

    // --- Capture Overlay ---
    Rectangle {
        id: captureOverlay
        anchors.fill: parent
        color: Qt.rgba(0, 0, 0, 0.8)
        visible: root.capturing
        focus: visible
        z: 50

        Column {
            anchors.centerIn: parent
            spacing: 24

            Text {
                text: "Press a face button, bumper, or stick click"
                font.pixelSize: Theme.fontBody
                color: Theme.textOnDark
                anchors.horizontalCenter: parent.horizontalCenter
            }
            Text {
                text: root.editingLabel
                font.pixelSize: Theme.fontTitle
                font.bold: true
                color: Theme.ember
                anchors.horizontalCenter: parent.horizontalCenter
            }
            Text {
                text: "Times out in 10 seconds"
                font.pixelSize: Theme.fontSmall
                color: Theme.textMuted
                anchors.horizontalCenter: parent.horizontalCenter
            }
        }

        Keys.onEscapePressed: {
            root.cancelCapture();
        }
    }
}
