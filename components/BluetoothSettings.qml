import QtQuick
import QtQuick.Layouts
import Quickshell.Io

// Bluetooth settings — rewired (Phase 3) to talk to the input daemon's D-Bus
// (bluer) backbone over the Unix socket instead of shelling out to
// `bluetoothctl`. Reads adapter power, lists devices, and drives
// connect/disconnect/pair/trust via IPC. Scan results arrive asynchronously as
// `bt:device` subscriber events.
//
// IPC commands used (see docs/IPC_PROTOCOL.md):
//   bt-power-status, bt-power-on, bt-power-off, bt-scan-on, bt-scan-off,
//   bt-list, bt-connect <mac>, bt-disconnect <mac>, bt-pair <mac>, bt-trust <mac>
// Streamed events consumed: bt:device:<json>, bt:device-removed:<mac>,
//   bt:powered:on|off, bt:scanning:on|off
FocusScope {
    id: root

    property bool powered: false
    property bool scanning: false
    property var pairedDevices: []
    property var availableDevices: []
    property string statusMessage: ""

    // All daemon IPC goes over native Quickshell sockets (SocketClient, #97) —
    // the python3 socket shims were retired in Phase 8. Request/response clients
    // read the first reply line; btEvents is a persistent `subscribe` stream
    // folding live `bt:*` scan events into the device lists.

    // Merge a device object (from bt-list or a bt:device event) into the
    // paired/available lists, partitioning by its `paired` flag. Replaces any
    // existing entry with the same mac (most-recent-wins) so live RSSI/state
    // updates from scan events refresh in place.
    function _mergeDevice(dev) {
        if (!dev || !dev.mac)
            return;
        // Strip from both lists first, then re-insert into the right bucket.
        let paired = [];
        for (let i = 0; i < root.pairedDevices.length; i++)
            if (root.pairedDevices[i].mac !== dev.mac)
                paired.push(root.pairedDevices[i]);
        let avail = [];
        for (let j = 0; j < root.availableDevices.length; j++)
            if (root.availableDevices[j].mac !== dev.mac)
                avail.push(root.availableDevices[j]);
        if (dev.paired)
            paired.push(dev);
        else
            avail.push(dev);
        root.pairedDevices = paired;
        root.availableDevices = avail;
    }

    function _removeDevice(mac) {
        let paired = [];
        for (let i = 0; i < root.pairedDevices.length; i++)
            if (root.pairedDevices[i].mac !== mac)
                paired.push(root.pairedDevices[i]);
        let avail = [];
        for (let j = 0; j < root.availableDevices.length; j++)
            if (root.availableDevices[j].mac !== mac)
                avail.push(root.availableDevices[j]);
        root.pairedDevices = paired;
        root.availableDevices = avail;
    }

    // --- Processes ---

    SocketClient {
        id: btPowerStatus
        onResponseReceived: line => {
            let t = line.trim();
            if (t === "bt:on")
                root.powered = true;
            else if (t === "bt:off")
                root.powered = false;
            // "error" leaves the prior state untouched.
            if (root.powered)
                btList.request("bt-list");
        }
    }

    SocketClient {
        id: btPowerOn
        onResponseReceived: response => {
            btPowerStatus.request("bt-power-status");
            pairedRefreshTimer.start();
        }
    }

    SocketClient {
        id: btPowerOff
        onResponseReceived: response => {
            root.scanning = false;
            root.availableDevices = [];
            btPowerStatus.request("bt-power-status");
        }
    }

    // Full device list — daemon returns a compact JSON array of
    // {mac,name,paired,connected,trusted,rssi}. Partition into paired/available.
    SocketClient {
        id: btList
        onResponseReceived: line => {
            try {
                let devs = JSON.parse(line);
                let paired = [];
                let avail = [];
                for (let i = 0; i < devs.length; i++) {
                    if (devs[i].paired)
                        paired.push(devs[i]);
                    else
                        avail.push(devs[i]);
                }
                root.pairedDevices = paired;
                root.availableDevices = avail;
            } catch (e) {
                console.log("BluetoothSettings: failed to parse bt-list:", e);
            }
        }
    }

    SocketClient {
        id: btScanOn
        // Gate the scanning state on the parsed reply line (the daemon replies
        // `ok` or `error:*`). Results stream as bt:device events via btEvents;
        // auto-stop after a fixed window.
        onResponseReceived: line => {
            if (line.trim() === "ok") {
                root.scanning = true;
                scanStopTimer.restart();
            } else {
                root.scanning = false;
                root.statusMessage = "Bluetooth unavailable";
            }
        }
        onRequestFailed: {
            root.scanning = false;
            root.statusMessage = "Bluetooth unavailable";
        }
    }

    SocketClient {
        id: btScanOff
        onResponseReceived: response => {
            root.scanning = false;
        }
        onRequestFailed: {
            root.scanning = false;
        }
    }

    SocketClient {
        id: btConnect
        property string mac: ""
        // Caller sets `mac`, then calls connect(); status is set here so the
        // "Connecting..." message shows immediately (the old onStarted hook).
        function connect(addr) {
            btConnect.mac = addr;
            root.statusMessage = "Connecting...";
            btConnect.request("bt-connect " + addr);
        }
        onResponseReceived: line => {
            root.statusMessage = line.trim() === "ok" ? "Connected" : "Connection failed";
            statusClearTimer.start();
            btList.request("bt-list");
        }
        onRequestFailed: {
            root.statusMessage = "Connection failed";
            statusClearTimer.start();
            btList.request("bt-list");
        }
    }

    SocketClient {
        id: btDisconnect
        property string mac: ""
        function disconnect(addr) {
            btDisconnect.mac = addr;
            root.statusMessage = "Disconnecting...";
            btDisconnect.request("bt-disconnect " + addr);
        }
        onResponseReceived: line => {
            root.statusMessage = line.trim() === "ok" ? "Disconnected" : "Disconnect failed";
            statusClearTimer.start();
            btList.request("bt-list");
        }
        onRequestFailed: {
            root.statusMessage = "Disconnect failed";
            statusClearTimer.start();
            btList.request("bt-list");
        }
    }

    SocketClient {
        id: btPair
        property string mac: ""
        function pair(addr) {
            btPair.mac = addr;
            root.statusMessage = "Pairing...";
            btPair.request("bt-pair " + addr);
        }
        function _finish() {
            statusClearTimer.start();
            // BlueZ just-works pairing; trust so it auto-reconnects, then refresh.
            if (btPair.mac !== "") {
                btTrust.mac = btPair.mac;
                btTrust.request("bt-trust " + btPair.mac);
            }
            btList.request("bt-list");
        }
        onResponseReceived: line => {
            root.statusMessage = line.trim() === "ok" ? "Paired" : "Pairing failed";
            btPair._finish();
        }
        onRequestFailed: {
            root.statusMessage = "Pairing failed";
            btPair._finish();
        }
    }

    SocketClient {
        id: btTrust
        property string mac: ""
    }

    // Live scan-result stream. Subscribed while the page is visible; folds
    // bt:device / bt:device-removed / bt:powered / bt:scanning events into state.
    SocketClient {
        id: btEvents
        subscribe: true
        onLineReceived: line => {
            let t = line.trim();
            if (t.startsWith("bt:device-removed:")) {
                root._removeDevice(t.substring(18));
            } else if (t.startsWith("bt:device:")) {
                try {
                    root._mergeDevice(JSON.parse(t.substring(10)));
                } catch (e) {
                    console.log("BluetoothSettings: failed to parse bt:device:", e);
                }
            } else if (t === "bt:powered:on") {
                root.powered = true;
                btList.request("bt-list");
            } else if (t === "bt:powered:off") {
                root.powered = false;
                root.scanning = false;
                root.availableDevices = [];
            } else if (t === "bt:scanning:on") {
                root.scanning = true;
            } else if (t === "bt:scanning:off") {
                root.scanning = false;
            }
        }
    }

    Timer {
        id: statusClearTimer
        interval: 3000
        onTriggered: {
            root.statusMessage = "";
        }
    }

    Timer {
        id: pairedRefreshTimer
        interval: 500
        onTriggered: {
            btList.request("bt-list");
        }
    }

    // Mirrors the previous `timeout 10 bluetoothctl scan on` window: stop the
    // daemon scan after 10s so the UI returns to idle.
    Timer {
        id: scanStopTimer
        interval: 10000
        onTriggered: {
            if (root.scanning)
                btScanOff.request("bt-scan-off");
        }
    }

    Component.onCompleted: {
        btPowerStatus.request("bt-power-status");
        btEvents.start();
    }

    onVisibleChanged: {
        if (visible) {
            btPowerStatus.request("bt-power-status");
            btEvents.start();
        }
    }

    function focusFirst() {
        powerToggleScope.forceActiveFocus();
    }

    ColumnLayout {
        anchors.fill: parent
        anchors.margins: Theme.padding
        spacing: 24

        // Power toggle
        RowLayout {
            Layout.fillWidth: true
            spacing: 24

            Text {
                text: "Bluetooth"
                font.pixelSize: Theme.fontBody
                font.bold: true
                color: Theme.textPrimary
            }

            Rectangle {
                Layout.preferredWidth: 160
                Layout.preferredHeight: 56
                radius: 28
                color: root.powered ? Theme.online : Theme.textSecondary

                Behavior on color {
                    ColorAnimation {
                        duration: 200
                    }
                }

                Text {
                    anchors.centerIn: parent
                    text: root.powered ? "ON" : "OFF"
                    font.pixelSize: Theme.fontSmall
                    font.bold: true
                    color: Theme.textOnDark
                }
            }

            FocusScope {
                id: powerToggleScope
                Layout.preferredWidth: powerToggleBtn.implicitWidth
                Layout.preferredHeight: powerToggleBtn.implicitHeight
                focus: true
                activeFocusOnTab: true

                KeyNavigation.down: scanScope
                KeyNavigation.right: scanScope

                SettingsButton {
                    id: powerToggleBtn
                    text: root.powered ? "Turn Off" : "Turn On"
                    focus: parent.activeFocus
                    anchors.fill: parent

                    MouseArea {
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: {
                            powerToggleScope.forceActiveFocus();
                            if (root.powered)
                                btPowerOff.request("bt-power-off");
                            else
                                btPowerOn.request("bt-power-on");
                        }
                    }
                }

                Keys.onReturnPressed: {
                    if (root.powered)
                        btPowerOff.request("bt-power-off");
                    else
                        btPowerOn.request("bt-power-on");
                }
            }

            FocusScope {
                id: scanScope
                Layout.preferredWidth: scanBtn.implicitWidth
                Layout.preferredHeight: scanBtn.implicitHeight
                activeFocusOnTab: true
                visible: root.powered

                KeyNavigation.left: powerToggleScope
                KeyNavigation.down: pairedList.count > 0 ? pairedList : availList

                SettingsButton {
                    id: scanBtn
                    text: root.scanning ? "Scanning..." : "Scan"
                    focus: parent.activeFocus
                    anchors.fill: parent

                    MouseArea {
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: {
                            scanScope.forceActiveFocus();
                            if (!root.scanning)
                                btScanOn.request("bt-scan-on");
                        }
                    }
                }

                Keys.onReturnPressed: {
                    if (!root.scanning)
                        btScanOn.request("bt-scan-on");
                }
            }

            Item {
                Layout.fillWidth: true
            }

            Text {
                text: root.statusMessage
                font.pixelSize: Theme.fontSmall
                color: Theme.gold
                visible: root.statusMessage !== ""
            }
        }

        // Paired devices
        Text {
            text: "Paired Devices"
            font.pixelSize: Theme.fontBody
            font.bold: true
            color: Theme.textPrimary
            visible: root.powered
        }

        ListView {
            id: pairedList
            Layout.fillWidth: true
            Layout.preferredHeight: Math.min(contentHeight, 300)
            spacing: 8
            clip: true
            visible: root.powered
            model: root.pairedDevices

            KeyNavigation.up: powerToggleScope
            KeyNavigation.down: availList.count > 0 ? availList : powerToggleScope

            delegate: Rectangle {
                required property int index
                required property var modelData
                width: pairedList.width
                height: 96
                radius: 16
                color: pairedList.currentIndex === index && pairedList.activeFocus ? Theme.surfaceHover : Theme.surface
                border.width: 2
                border.color: modelData.connected ? Theme.online : Theme.surfaceBorder

                Behavior on color {
                    ColorAnimation {
                        duration: 150
                    }
                }

                RowLayout {
                    anchors.fill: parent
                    anchors.leftMargin: 24
                    anchors.rightMargin: 24
                    anchors.topMargin: 16
                    anchors.bottomMargin: 16
                    spacing: 16

                    Rectangle {
                        Layout.preferredWidth: 20
                        Layout.preferredHeight: 20
                        radius: 10
                        color: modelData.connected ? Theme.online : Theme.textSecondary
                    }

                    Text {
                        text: modelData.name
                        font.pixelSize: Theme.fontSmall
                        color: Theme.textPrimary
                        Layout.fillWidth: true
                    }

                    Text {
                        text: modelData.connected ? "Connected" : "Disconnected"
                        font.pixelSize: Theme.fontSmall
                        color: Theme.textSecondary
                    }
                }

                MouseArea {
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: {
                        pairedList.currentIndex = index;
                        pairedList.forceActiveFocus();
                    }
                    onDoubleClicked: {
                        if (modelData.connected) {
                            btDisconnect.disconnect(modelData.mac);
                        } else {
                            btConnect.connect(modelData.mac);
                        }
                    }
                }
            }

            Keys.onReturnPressed: {
                if (currentIndex >= 0 && currentIndex < root.pairedDevices.length) {
                    let dev = root.pairedDevices[currentIndex];
                    if (dev.connected) {
                        btDisconnect.disconnect(dev.mac);
                    } else {
                        btConnect.connect(dev.mac);
                    }
                }
            }
        }

        Text {
            text: root.pairedDevices.length === 0 && root.powered ? "No paired devices" : ""
            font.pixelSize: Theme.fontSmall
            color: Theme.textSecondary
            visible: text !== ""
        }

        // Available (unpaired) devices
        Text {
            text: "Available Devices"
            font.pixelSize: Theme.fontBody
            font.bold: true
            color: Theme.textPrimary
            visible: root.powered && root.availableDevices.length > 0
        }

        ListView {
            id: availList
            Layout.fillWidth: true
            Layout.fillHeight: true
            spacing: 8
            clip: true
            visible: root.powered && root.availableDevices.length > 0
            model: root.availableDevices

            KeyNavigation.up: pairedList.count > 0 ? pairedList : powerToggleScope

            delegate: Rectangle {
                required property int index
                required property var modelData
                width: availList.width
                height: 96
                radius: 16
                color: availList.currentIndex === index && availList.activeFocus ? Theme.surfaceHover : Theme.surface
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
                    anchors.topMargin: 16
                    anchors.bottomMargin: 16
                    spacing: 16

                    Text {
                        text: modelData.name
                        font.pixelSize: Theme.fontSmall
                        color: Theme.textPrimary
                        Layout.fillWidth: true
                    }

                    Text {
                        text: modelData.mac
                        font.pixelSize: Theme.fontSmall
                        color: Theme.textSecondary
                    }
                }

                MouseArea {
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: {
                        availList.currentIndex = index;
                        availList.forceActiveFocus();
                    }
                    onDoubleClicked: {
                        btPair.pair(modelData.mac);
                    }
                }
            }

            Keys.onReturnPressed: {
                if (currentIndex >= 0 && currentIndex < root.availableDevices.length) {
                    btPair.pair(root.availableDevices[currentIndex].mac);
                }
            }
        }

        // Hint
        Text {
            text: root.powered ? "A: Connect/Disconnect  |  Scan to find new devices" : "Turn on Bluetooth to manage devices"
            font.pixelSize: Theme.fontHint
            color: Theme.textSecondary
            Layout.alignment: Qt.AlignHCenter
        }
    }
}
