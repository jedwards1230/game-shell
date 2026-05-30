import QtQuick
import QtQuick.Layouts
import Quickshell.Io

// KDE-style full-screen power chooser. Modal overlay following the
// NotificationCenter pattern (FocusScope + DimmedBackdrop + modal key
// capture). Offers Sleep / Restart / Shut Down / Logout / Cancel in a
// centered horizontal row. Default focus lands on Cancel so an accidental
// controller-A press never triggers a destructive action.
FocusScope {
    id: root

    property bool opened: false
    // Default to the last index (Cancel) for safety.
    property int _selectedIndex: 4

    signal cancelled

    visible: opened
    anchors.fill: parent
    focus: opened

    // === Power commands (copied from PowerSettings) ===
    // Suspend routes through the input daemon's logind-over-zbus backbone
    // (Phase 3) over a native Quickshell socket (SocketClient, #97). Reboot/
    // poweroff stay one-shot `systemctl`; logout stays `hyprctl dispatch exit`.
    SocketClient {
        id: suspendCmd
    }
    Process {
        id: rebootCmd
        command: ["systemctl", "reboot"]
    }
    Process {
        id: powerOffCmd
        command: ["systemctl", "poweroff"]
    }
    // Logout = cleanly exit the Hyprland session. hyprctl inherits the
    // session env (HYPRLAND_INSTANCE_SIGNATURE / XDG_RUNTIME_DIR) from the
    // running Quickshell process, like AppLifecycleManager's hyprctl calls.
    Process {
        id: logoutCmd
        command: ["hyprctl", "dispatch", "exit"]
    }

    readonly property var _actions: [
        {
            label: "Sleep",
            icon: "system-suspend",
            fallback: "☾",
            color: Theme.gold
        },
        {
            label: "Restart",
            icon: "system-reboot",
            fallback: "↻",
            color: Theme.ember
        },
        {
            label: "Shut Down",
            icon: "system-shutdown",
            fallback: "⏻",
            color: Theme.crimson
        },
        {
            label: "Logout",
            icon: "system-log-out",
            fallback: "⏻",
            color: Theme.navy
        },
        {
            label: "Cancel",
            icon: "dialog-cancel",
            fallback: "✕",
            color: Theme.surfaceHover
        }
    ]

    function _activate(index) {
        switch (index) {
        case 0:
            suspendCmd.request("power-suspend");
            break;
        case 1:
            rebootCmd.running = true;
            break;
        case 2:
            powerOffCmd.running = true;
            break;
        case 3:
            logoutCmd.running = true;
            break;
        case 4:
        default:
            root.cancelled();
            break;
        }
    }

    onOpenedChanged: {
        if (opened) {
            _selectedIndex = 4;
            root.forceActiveFocus();
        }
    }

    Keys.onPressed: event => {
        if (event.key === Qt.Key_Left) {
            if (_selectedIndex > 0)
                _selectedIndex--;
        } else if (event.key === Qt.Key_Right) {
            if (_selectedIndex < _actions.length - 1)
                _selectedIndex++;
        } else if (event.key === Qt.Key_Return || event.key === Qt.Key_Enter) {
            root._activate(_selectedIndex);
        } else if (event.key === Qt.Key_Escape) {
            root.cancelled();
        }
        // Modal: block all keys from reaching items below.
        event.accepted = true;
    }

    // Backdrop — near-opaque so the chooser reads as its own window
    // on top of everything else, not a translucent layer.
    DimmedBackdrop {
        dimLevel: 0.97
        onClicked: root.cancelled()
    }

    // Action row
    RowLayout {
        anchors.centerIn: parent
        spacing: Units.gridUnit * 1.5

        Repeater {
            model: root._actions

            FocusScope {
                id: actionScope
                required property int index
                required property var modelData

                Layout.preferredWidth: Units.gridUnit * 5
                Layout.preferredHeight: Units.gridUnit * 6

                readonly property bool selected: root._selectedIndex === index

                ColumnLayout {
                    anchors.centerIn: parent
                    spacing: Units.spacingMD

                    // Circular icon button
                    Rectangle {
                        Layout.alignment: Qt.AlignHCenter
                        Layout.preferredWidth: Units.gridUnit * 3.5
                        Layout.preferredHeight: Units.gridUnit * 3.5
                        radius: width / 2
                        color: actionScope.selected ? modelData.color : Theme.surface
                        border.width: actionScope.selected ? 0 : Units.borderMedium
                        border.color: Theme.surfaceBorder

                        Behavior on color {
                            ColorAnimation {
                                duration: 150
                            }
                        }

                        Image {
                            id: actionIcon
                            anchors.centerIn: parent
                            source: IconTheme.base ? "file://" + IconTheme.base + "/actions/22/" + modelData.icon + ".svg" : ""
                            sourceSize: Qt.size(Units.iconSizeMD, Units.iconSizeMD)
                            width: Units.iconSizeMD
                            height: Units.iconSizeMD
                            fillMode: Image.PreserveAspectFit
                            visible: status === Image.Ready
                        }
                        Text {
                            anchors.centerIn: parent
                            text: modelData.fallback
                            font.pixelSize: Units.iconSizeMD
                            color: actionScope.selected ? Theme.textOnDark : Theme.textSecondary
                            visible: actionIcon.status !== Image.Ready
                        }

                        MouseArea {
                            anchors.fill: parent
                            hoverEnabled: true
                            cursorShape: Qt.PointingHandCursor
                            onEntered: root._selectedIndex = actionScope.index
                            onClicked: root._activate(actionScope.index)
                        }
                    }

                    // Label
                    Text {
                        Layout.alignment: Qt.AlignHCenter
                        text: modelData.label
                        font.pixelSize: Theme.fontBody
                        font.bold: actionScope.selected
                        color: actionScope.selected ? Theme.textPrimary : Theme.textMuted
                    }
                }
            }
        }
    }

    // Bottom hint bar
    Text {
        anchors.horizontalCenter: parent.horizontalCenter
        anchors.bottom: parent.bottom
        anchors.bottomMargin: Units.gridUnit * 2
        text: "D-pad: Navigate    A: Select    B: Cancel"
        font.pixelSize: Theme.fontHint
        color: Theme.textMuted
    }
}
