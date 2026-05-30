import QtQuick
import QtQuick.Layouts
import Quickshell.Io

// IPC protocol: see docs/IPC_PROTOCOL.md
// Commands used: status, grab, release
FocusScope {
    id: root

    property var controllers: []
    property bool daemonRunning: false
    property bool daemonConnected: false
    property bool daemonGrabbed: false

    // --- Device Discovery ---
    //
    // NOTE: this is an evdev/`/proc/bus/input/devices` enumerator, NOT a daemon
    // Unix-socket shim, so it was intentionally left as a `python3 -c` Process by
    // the Phase 8 (#97) socket cutover. The daemon's `get-pads` IPC only reports
    // the grabbed fleet ({id,index,name,grabbed}); this diagnostic page lists ALL
    // controller-like input devices the system sees (incl. ungrabbed/virtual),
    // with vendor/product/path/phys detail `get-pads` does not carry. Converting
    // it to a socket call would change what the page shows. See followups.
    Process {
        id: scanControllers
        command: ["python3", "-c", `
import json
try:
    import evdev
    devs = []
    for p in evdev.list_devices():
        d = evdev.InputDevice(p)
        caps = d.capabilities(verbose=True)
        has_gamepad = any('BTN_GAMEPAD' in str(c) or 'BTN_SOUTH' in str(c) or 'ABS_X' in str(c) for cc in caps.values() for c in cc)
        if has_gamepad or 'gamepad' in d.name.lower() or 'controller' in d.name.lower() or 'xbox' in d.name.lower() or 'joystick' in d.name.lower():
            devs.append({'name': d.name, 'path': d.path, 'vendor': hex(d.info.vendor), 'product': hex(d.info.product), 'phys': d.phys or ''})
    print(json.dumps(devs))
except ImportError:
    import re
    devs = []
    with open('/proc/bus/input/devices') as f:
        blocks = f.read().split('\\n\\n')
    for block in blocks:
        if not block.strip(): continue
        name_m = re.search(r'N: Name="(.+)"', block)
        handler_m = re.search(r'H: Handlers=.*(event\\d+)', block)
        vendor_m = re.search(r'Vendor=(\\w+)', block)
        product_m = re.search(r'Product=(\\w+)', block)
        if name_m and handler_m:
            n = name_m.group(1).lower()
            if any(k in n for k in ['gamepad', 'controller', 'xbox', 'joystick', 'game']):
                devs.append({'name': name_m.group(1), 'path': '/dev/input/' + handler_m.group(1), 'vendor': '0x' + (vendor_m.group(1) if vendor_m else '0000'), 'product': '0x' + (product_m.group(1) if product_m else '0000'), 'phys': ''})
    print(json.dumps(devs))
`]
        stdout: SplitParser {
            onRead: line => {
                try {
                    root.controllers = JSON.parse(line);
                } catch (e) {
                    console.log("ControllerSettings: failed to parse controllers:", e);
                }
            }
        }
    }

    // --- Daemon Status ---

    SocketClient {
        id: daemonStatus
        onResponseReceived: line => {
            // Format: "connected:grabbed" or "disconnected:released"
            let parts = line.split(":");
            root.daemonConnected = parts[0] === "connected";
            root.daemonGrabbed = parts.length > 1 && parts[1] === "grabbed";
            root.daemonRunning = true;
        }
        onRequestFailed: {
            // Socket connect failed -> the daemon isn't reachable.
            root.daemonRunning = false;
            root.daemonConnected = false;
            root.daemonGrabbed = false;
        }
    }

    // --- Grab / Release ---

    SocketClient {
        id: grabCmd
        onResponseReceived: response => daemonStatus.request("status")
        onRequestFailed: daemonStatus.request("status")
    }

    SocketClient {
        id: releaseCmd
        onResponseReceived: response => daemonStatus.request("status")
        onRequestFailed: daemonStatus.request("status")
    }

    // --- Auto-refresh ---

    Timer {
        id: autoRefresh
        interval: 10000
        running: root.visible
        repeat: true
        onTriggered: {
            scanControllers.running = true;
            daemonStatus.request("status");
        }
    }

    Component.onCompleted: {
        scanControllers.running = true;
        daemonStatus.request("status");
    }

    onVisibleChanged: {
        if (visible) {
            scanControllers.running = true;
            daemonStatus.request("status");
        }
    }

    // Focus first actionable element.
    function focusFirst() {
        if (root.controllers.length > 0)
            controllerList.forceActiveFocus();
        else
            refreshScope.forceActiveFocus();
    }

    // --- Layout ---

    ColumnLayout {
        anchors.fill: parent
        anchors.margins: Theme.padding
        spacing: 32

        // Connected Controllers header + refresh
        RowLayout {
            Layout.fillWidth: true
            spacing: 24

            Text {
                text: "Connected Controllers"
                font.pixelSize: Theme.fontBody
                font.bold: true
                color: Theme.textPrimary
                Layout.fillWidth: true
            }

            FocusScope {
                id: refreshScope
                width: refreshBtn.width
                height: refreshBtn.height
                activeFocusOnTab: true

                KeyNavigation.down: root.controllers.length > 0 ? controllerList : grabScope

                SettingsButton {
                    id: refreshBtn
                    text: "Refresh"
                    focus: parent.activeFocus
                    anchors.fill: parent

                    MouseArea {
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: {
                            refreshScope.forceActiveFocus();
                            scanControllers.running = true;
                            daemonStatus.request("status");
                        }
                    }
                }

                Keys.onReturnPressed: {
                    scanControllers.running = true;
                    daemonStatus.request("status");
                }
            }
        }

        // Controller list
        ListView {
            id: controllerList
            Layout.fillWidth: true
            Layout.preferredHeight: Math.min(Math.max(root.controllers.length, 1) * 200, 600)
            spacing: 16
            clip: true
            model: root.controllers
            focus: true

            KeyNavigation.up: refreshScope
            KeyNavigation.down: grabScope

            delegate: Rectangle {
                required property int index
                required property var modelData
                width: controllerList.width
                height: 180
                radius: 16
                color: controllerList.currentIndex === index && controllerList.activeFocus ? Theme.surfaceHover : Theme.surface
                border.width: 2
                border.color: Theme.surfaceBorder

                Behavior on color {
                    ColorAnimation {
                        duration: 150
                    }
                }

                ColumnLayout {
                    anchors.fill: parent
                    anchors.leftMargin: 24
                    anchors.rightMargin: 24
                    anchors.topMargin: 20
                    anchors.bottomMargin: 20
                    spacing: 8

                    Text {
                        text: modelData.name
                        font.pixelSize: Theme.fontBody
                        font.bold: true
                        color: Theme.textPrimary
                        elide: Text.ElideRight
                        Layout.fillWidth: true
                    }

                    RowLayout {
                        spacing: 32

                        Text {
                            text: "Device: " + modelData.path
                            font.pixelSize: Theme.fontSmall
                            color: Theme.textSecondary
                        }

                        Text {
                            text: "ID: " + modelData.vendor + ":" + modelData.product
                            font.pixelSize: Theme.fontSmall
                            color: Theme.textSecondary
                        }
                    }

                    Text {
                        text: modelData.phys ? "Phys: " + modelData.phys : ""
                        font.pixelSize: Theme.fontSmall
                        color: Theme.textMuted
                        visible: modelData.phys !== ""
                        elide: Text.ElideRight
                        Layout.fillWidth: true
                    }
                }
            }

            // Empty state
            Text {
                anchors.centerIn: parent
                text: "No controllers detected"
                font.pixelSize: Theme.fontBody
                color: Theme.textMuted
                visible: root.controllers.length === 0
            }
        }

        // --- Input Daemon Status ---

        Text {
            text: "Input Daemon"
            font.pixelSize: Theme.fontBody
            font.bold: true
            color: Theme.textPrimary
        }

        RowLayout {
            Layout.fillWidth: true
            spacing: 24

            // Status indicator
            Rectangle {
                width: 24
                height: 24
                radius: 12
                color: root.daemonRunning && root.daemonConnected ? Theme.online : Theme.offline
            }

            Text {
                text: {
                    if (!root.daemonRunning)
                        return "Daemon not running";
                    if (!root.daemonConnected)
                        return "No controller connected";
                    return root.daemonGrabbed ? "Controller connected — Grabbed (shell has exclusive input)" : "Controller connected — Released (raw input to apps)";
                }
                font.pixelSize: Theme.fontSmall
                color: Theme.textSecondary
                Layout.fillWidth: true
            }

            FocusScope {
                id: grabScope
                width: grabBtn.width
                height: grabBtn.height
                activeFocusOnTab: true

                KeyNavigation.up: controllerList
                KeyNavigation.down: debugScope

                SettingsButton {
                    id: grabBtn
                    text: root.daemonGrabbed ? "Release" : "Grab"
                    focus: parent.activeFocus
                    anchors.fill: parent

                    MouseArea {
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: {
                            grabScope.forceActiveFocus();
                            if (root.daemonGrabbed) {
                                releaseCmd.request("release");
                            } else {
                                grabCmd.request("grab");
                            }
                        }
                    }
                }

                Keys.onReturnPressed: {
                    if (root.daemonGrabbed) {
                        releaseCmd.request("release");
                    } else {
                        grabCmd.request("grab");
                    }
                }
            }
        }

        // --- Debug Input Toggle ---

        Text {
            text: "Debug"
            font.pixelSize: Theme.fontBody
            font.bold: true
            color: Theme.textPrimary
        }

        RowLayout {
            Layout.fillWidth: true
            spacing: 24

            Text {
                text: "Show input debug overlay"
                font.pixelSize: Theme.fontSmall
                color: Theme.textSecondary
                Layout.fillWidth: true
            }

            FocusScope {
                id: debugScope
                width: debugBtn.width
                height: debugBtn.height
                activeFocusOnTab: true

                KeyNavigation.up: grabScope

                SettingsButton {
                    id: debugBtn
                    text: Theme.controllerDebug ? "Disable" : "Enable"
                    focus: parent.activeFocus
                    anchors.fill: parent

                    color: Theme.controllerDebug ? Theme.sidebarActive : (parent.activeFocus ? Theme.surfaceHover : Theme.surface)

                    MouseArea {
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: {
                            debugScope.forceActiveFocus();
                            Theme.setControllerDebug(!Theme.controllerDebug);
                        }
                    }
                }

                Keys.onReturnPressed: {
                    Theme.setControllerDebug(!Theme.controllerDebug);
                }
            }
        }

        Item {
            Layout.fillHeight: true
        }

        Text {
            text: "A: Select  |  Auto-refreshes every 10s"
            font.pixelSize: Theme.fontHint
            color: Theme.textSecondary
            Layout.alignment: Qt.AlignHCenter
        }
    }
}
