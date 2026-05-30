import QtQuick
import QtQuick.Layouts

// IPC protocol: see docs/IPC_PROTOCOL.md
// Commands used: status, grab, release, list-input-devices, get-pads,
//                get-config, set-config (rumbleEnabled via SettingsStore)
// Events consumed (subscribe): pad:connected / pad:disconnected / pad:index /
//                pad:battery — folded into the live `pads` fleet model.
FocusScope {
    id: root

    // Diagnostic enumeration of EVERY controller-like input device the system
    // sees (incl. ungrabbed/virtual), from the daemon `list-input-devices` IPC.
    // Shape per element: {name,path,vendor,product,phys,handlers,grabbed}.
    property var controllers: []

    // Live fleet of pads the daemon currently owns, with player slot + battery,
    // built from get-pads (seed) + pad:* events (deltas). Shape per element:
    // {id,index,name,batteryLevel,batteryCharging}. batteryLevel === -1 means
    // the pad reports no battery (wired) — render no glyph, not "0%".
    property var pads: []

    property bool daemonRunning: false
    property bool daemonConnected: false
    property bool daemonGrabbed: false

    // --- Device Discovery (#97 — last python3 shim removed) ---
    //
    // Diagnostic enumerator of ALL controller-like input devices (incl.
    // ungrabbed/virtual), now served by the daemon's `list-input-devices` IPC
    // (replaces the old evdev/`/proc/bus/input/devices` python3 reader). The
    // reply is a single-line JSON array of
    // {name,path,vendor,product,phys,handlers,grabbed} objects.
    SocketClient {
        id: scanControllers
        onResponseReceived: line => {
            try {
                var list = JSON.parse(line);
                if (Array.isArray(list))
                    root.controllers = list;
            } catch (e) {
                console.log("ControllerSettings: failed to parse list-input-devices:", e);
            }
        }
        onRequestFailed: root.controllers = []
    }

    function scanDevices() {
        scanControllers.request("list-input-devices");
    }

    // --- Connected fleet model (#100 battery / #101 index) ---
    //
    // Seeded by get-pads, kept live by the pad:* subscribe stream below.
    SocketClient {
        id: getPads
        onResponseReceived: line => {
            try {
                var list = JSON.parse(line);
                if (Array.isArray(list))
                    root._setPadsFromList(list);
            } catch (e) {
                console.log("ControllerSettings: failed to parse get-pads:", e);
            }
        }
    }

    SocketClient {
        id: padEvents
        subscribe: true
        onLineReceived: line => {
            if (line.startsWith("pad:connected:"))
                root._handlePadJson(line.substring(14), root._padConnected);
            else if (line.startsWith("pad:index:"))
                root._handlePadJson(line.substring(10), root._padIndex);
            else if (line.startsWith("pad:battery:"))
                root._handlePadJson(line.substring(12), root._padBattery);
            else if (line.startsWith("pad:disconnected:"))
                root._padDisconnected(line.substring(17));
            else if (line === "controller-wake")
                root.refreshPads();
        }
    }

    function refreshPads() {
        getPads.request("get-pads");
    }

    function _padIndexById(id) {
        for (var i = 0; i < root.pads.length; i++) {
            if (root.pads[i].id === id)
                return i;
        }
        return -1;
    }

    function _setPadsFromList(list) {
        var next = [];
        for (var i = 0; i < list.length; i++) {
            var d = list[i];
            var prev = root._padIndexById(d.id);
            next.push({
                "id": d.id,
                "index": d.index,
                "name": d.name,
                "batteryLevel": prev >= 0 ? root.pads[prev].batteryLevel : -1,
                "batteryCharging": prev >= 0 ? root.pads[prev].batteryCharging : false
            });
        }
        next.sort((a, b) => a.index - b.index);
        root.pads = next;
    }

    function _handlePadJson(json, apply) {
        try {
            apply(JSON.parse(json));
        } catch (e) {
            console.log("ControllerSettings: failed to parse pad event:", e);
        }
    }

    function _padConnected(obj) {
        var next = root.pads.slice();
        var i = root._padIndexById(obj.id);
        if (i >= 0) {
            next[i] = Object.assign({}, next[i], {
                "index": obj.index,
                "name": obj.name
            });
        } else {
            next.push({
                "id": obj.id,
                "index": obj.index,
                "name": obj.name,
                "batteryLevel": -1,
                "batteryCharging": false
            });
        }
        next.sort((a, b) => a.index - b.index);
        root.pads = next;
    }

    function _padIndex(obj) {
        var i = root._padIndexById(obj.id);
        if (i < 0)
            return;
        var next = root.pads.slice();
        next[i] = Object.assign({}, next[i], {
            "index": obj.index
        });
        next.sort((a, b) => a.index - b.index);
        root.pads = next;
    }

    function _padBattery(obj) {
        var i = root._padIndexById(obj.id);
        if (i < 0)
            return;
        var next = root.pads.slice();
        next[i] = Object.assign({}, next[i], {
            "batteryLevel": obj.level,
            "batteryCharging": obj.charging
        });
        root.pads = next;
    }

    function _padDisconnected(id) {
        var next = [];
        for (var i = 0; i < root.pads.length; i++) {
            if (root.pads[i].id !== id)
                next.push(root.pads[i]);
        }
        root.pads = next;
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
            root.scanDevices();
            root.refreshPads();
            daemonStatus.request("status");
        }
    }

    Component.onCompleted: {
        padEvents.start();
        root.scanDevices();
        root.refreshPads();
        daemonStatus.request("status");
    }

    onVisibleChanged: {
        if (visible) {
            root.scanDevices();
            root.refreshPads();
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

        // === Connected Controllers (fleet — player slot + battery) ===
        Text {
            text: "Connected Controllers"
            font.pixelSize: Theme.fontBody
            font.bold: true
            color: Theme.textPrimary
            Layout.fillWidth: true
        }

        // Live fleet from the daemon (get-pads + pad:* events). Each row shows
        // the player slot (P1/P2…), the device name, and — for pads that report
        // a battery (wireless) — a battery glyph + percentage with a charging
        // bolt. Wired pads (batteryLevel === -1) show no battery, not "0%".
        ColumnLayout {
            Layout.fillWidth: true
            spacing: 12

            Repeater {
                model: root.pads

                delegate: Rectangle {
                    required property var modelData
                    Layout.fillWidth: true
                    Layout.preferredHeight: 96
                    radius: 16
                    color: Theme.surface
                    border.width: 2
                    border.color: Theme.surfaceBorder

                    RowLayout {
                        anchors.fill: parent
                        anchors.leftMargin: 24
                        anchors.rightMargin: 24
                        spacing: 24

                        // Player slot badge (P1, P2, …)
                        Rectangle {
                            Layout.preferredWidth: 64
                            Layout.preferredHeight: 64
                            radius: 12
                            color: Theme.sidebarActive

                            Text {
                                anchors.centerIn: parent
                                text: "P" + (modelData.index + 1)
                                font.pixelSize: Theme.fontBody
                                font.bold: true
                                color: Theme.textPrimary
                            }
                        }

                        Text {
                            text: modelData.name
                            font.pixelSize: Theme.fontBody
                            font.bold: true
                            color: Theme.textPrimary
                            elide: Text.ElideRight
                            Layout.fillWidth: true
                        }

                        // Battery — only when the pad reports one (wireless).
                        RowLayout {
                            spacing: 8
                            visible: modelData.batteryLevel >= 0

                            // Charging bolt
                            Text {
                                text: "⚡"
                                font.pixelSize: Theme.fontSmall
                                color: Theme.warning
                                visible: modelData.batteryCharging === true
                            }

                            Text {
                                text: "\u{1F50B}"
                                font.pixelSize: Theme.fontSmall
                                color: Theme.textSecondary
                            }

                            Text {
                                text: modelData.batteryLevel + "%"
                                font.pixelSize: Theme.fontSmall
                                font.bold: true
                                color: {
                                    if (modelData.batteryLevel <= 15)
                                        return Theme.offline;
                                    return Theme.textSecondary;
                                }
                            }
                        }
                    }
                }
            }

            // Empty state for the fleet.
            Text {
                text: "No controllers connected"
                font.pixelSize: Theme.fontSmall
                color: Theme.textMuted
                visible: root.pads.length === 0
            }
        }

        // === Detected Input Devices (diagnostic enumerator) ===
        RowLayout {
            Layout.fillWidth: true
            spacing: 24

            Text {
                text: "Detected Input Devices"
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
                            root.scanDevices();
                            root.refreshPads();
                            daemonStatus.request("status");
                        }
                    }
                }

                Keys.onReturnPressed: {
                    root.scanDevices();
                    root.refreshPads();
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

                        // Grabbed badge — only for pads the fleet owns.
                        Rectangle {
                            Layout.preferredHeight: 28
                            Layout.preferredWidth: grabbedLabel.implicitWidth + 24
                            radius: 8
                            color: Theme.sidebarActive
                            visible: modelData.grabbed === true

                            Text {
                                id: grabbedLabel
                                anchors.centerIn: parent
                                text: "Grabbed"
                                font.pixelSize: Theme.fontHint
                                font.bold: true
                                color: Theme.textPrimary
                            }
                        }
                    }

                    Text {
                        text: {
                            var parts = [];
                            if (modelData.phys)
                                parts.push("Phys: " + modelData.phys);
                            var h = modelData.handlers;
                            if (h && h.length > 0)
                                parts.push("Handlers: " + h.join(", "));
                            return parts.join("    ");
                        }
                        font.pixelSize: Theme.fontSmall
                        color: Theme.textMuted
                        visible: text !== ""
                        elide: Text.ElideRight
                        Layout.fillWidth: true
                    }
                }
            }

            // Empty state
            Text {
                anchors.centerIn: parent
                text: "No input devices detected"
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
                KeyNavigation.down: rumbleScope

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

        // --- Rumble Toggle (#99) ---

        Text {
            text: "Rumble"
            font.pixelSize: Theme.fontBody
            font.bold: true
            color: Theme.textPrimary
        }

        RowLayout {
            Layout.fillWidth: true
            spacing: 24

            Text {
                text: "Controller rumble / haptics"
                font.pixelSize: Theme.fontSmall
                color: Theme.textSecondary
                Layout.fillWidth: true
            }

            FocusScope {
                id: rumbleScope
                width: rumbleBtn.width
                height: rumbleBtn.height
                activeFocusOnTab: true

                KeyNavigation.up: debugScope

                SettingsButton {
                    id: rumbleBtn
                    text: SettingsStore.rumbleEnabled ? "Disable" : "Enable"
                    focus: parent.activeFocus
                    anchors.fill: parent

                    color: SettingsStore.rumbleEnabled ? Theme.sidebarActive : (parent.activeFocus ? Theme.surfaceHover : Theme.surface)

                    MouseArea {
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: {
                            rumbleScope.forceActiveFocus();
                            SettingsStore.setRumbleEnabled(!SettingsStore.rumbleEnabled);
                        }
                    }
                }

                Keys.onReturnPressed: {
                    SettingsStore.setRumbleEnabled(!SettingsStore.rumbleEnabled);
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
