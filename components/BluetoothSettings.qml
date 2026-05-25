import QtQuick
import QtQuick.Layouts
import Quickshell.Io

FocusScope {
    id: root

    property bool powered: false
    property bool scanning: false
    property var pairedDevices: []
    property var availableDevices: []
    property string statusMessage: ""

    // --- Processes ---

    Process {
        id: btPowerStatus
        command: ["bluetoothctl", "show"]
        stdout: SplitParser {
            onRead: (line) => {
                if (line.indexOf("Powered:") >= 0) {
                    root.powered = line.indexOf("yes") >= 0
                }
            }
        }
    }

    Process {
        id: btPowerOn
        command: ["bluetoothctl", "power", "on"]
        onExited: {
            btPowerStatus.running = true
            pairedRefreshTimer.start()
        }
    }

    Process {
        id: btPowerOff
        command: ["bluetoothctl", "power", "off"]
        onExited: {
            root.scanning = false
            btPowerStatus.running = true
        }
    }

    Process {
        id: btListPaired
        command: ["bluetoothctl", "devices", "Paired"]
        stdout: SplitParser {
            property var collected: []
            onRead: (line) => {
                // "Device AA:BB:CC:DD:EE:FF Device Name"
                let match = line.trim().match(/^Device\s+([0-9A-Fa-f:]+)\s+(.+)$/)
                if (match) {
                    collected.push({ mac: match[1], name: match[2] })
                }
            }
        }
        onExited: {
            root.pairedDevices = btListPaired.stdout.collected
            btListPaired.stdout.collected = []
            // Check connection status for each
            if (root.pairedDevices.length > 0) {
                btInfoDevice.mac = root.pairedDevices[0].mac
                btInfoDevice.deviceIndex = 0
                btInfoDevice.running = true
            }
        }
    }

    Process {
        id: btInfoDevice
        property string mac: ""
        property int deviceIndex: 0
        command: ["bluetoothctl", "info", mac]
        stdout: SplitParser {
            onRead: (line) => {
                if (line.indexOf("Connected:") >= 0) {
                    let connected = line.indexOf("yes") >= 0
                    let idx = btInfoDevice.deviceIndex
                    if (idx >= 0 && idx < root.pairedDevices.length) {
                        let devs = root.pairedDevices
                        devs[idx].connected = connected
                        root.pairedDevices = devs
                    }
                }
            }
        }
        onExited: {
            let next = btInfoDevice.deviceIndex + 1
            if (next < root.pairedDevices.length) {
                btInfoDevice.mac = root.pairedDevices[next].mac
                btInfoDevice.deviceIndex = next
                btInfoDevice.running = true
            }
        }
    }

    Process {
        id: btScanOn
        command: ["bash", "-c", "timeout 10 bluetoothctl scan on 2>&1 || true"]
        onStarted: { root.scanning = true }
        onExited: {
            root.scanning = false
            btListAvailable.running = true
        }
    }

    Process {
        id: btListAvailable
        command: ["bluetoothctl", "devices"]
        stdout: SplitParser {
            property var collected: []
            onRead: (line) => {
                let match = line.trim().match(/^Device\s+([0-9A-Fa-f:]+)\s+(.+)$/)
                if (match) {
                    // Exclude already paired
                    let isPaired = false
                    for (let i = 0; i < root.pairedDevices.length; i++) {
                        if (root.pairedDevices[i].mac === match[1]) {
                            isPaired = true
                            break
                        }
                    }
                    if (!isPaired) {
                        collected.push({ mac: match[1], name: match[2] })
                    }
                }
            }
        }
        onExited: {
            root.availableDevices = btListAvailable.stdout.collected
            btListAvailable.stdout.collected = []
        }
    }

    Process {
        id: btConnect
        property string mac: ""
        command: ["bluetoothctl", "connect", mac]
        onStarted: { root.statusMessage = "Connecting..." }
        onExited: (exitCode) => {
            root.statusMessage = exitCode === 0 ? "Connected" : "Connection failed"
            statusClearTimer.start()
            btListPaired.running = true
        }
    }

    Process {
        id: btDisconnect
        property string mac: ""
        command: ["bluetoothctl", "disconnect", mac]
        onStarted: { root.statusMessage = "Disconnecting..." }
        onExited: (exitCode) => {
            root.statusMessage = exitCode === 0 ? "Disconnected" : "Disconnect failed"
            statusClearTimer.start()
            btListPaired.running = true
        }
    }

    Process {
        id: btPair
        property string mac: ""
        command: ["bash", "-c", "echo 'yes' | bluetoothctl pair " + mac]
        onStarted: { root.statusMessage = "Pairing..." }
        onExited: (exitCode) => {
            root.statusMessage = exitCode === 0 ? "Paired" : "Pairing failed"
            statusClearTimer.start()
            btListPaired.running = true
        }
    }

    Timer {
        id: statusClearTimer
        interval: 3000
        onTriggered: { root.statusMessage = "" }
    }

    Timer {
        id: pairedRefreshTimer
        interval: 500
        onTriggered: { btListPaired.running = true }
    }

    Component.onCompleted: {
        btPowerStatus.running = true
        btListPaired.running = true
    }

    onVisibleChanged: {
        if (visible) {
            btPowerStatus.running = true
            btListPaired.running = true
            powerToggleScope.forceActiveFocus()
        }
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
                width: 160
                height: 56
                radius: 28
                color: root.powered ? Theme.online : Theme.textSecondary

                Behavior on color { ColorAnimation { duration: 200 } }

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
                width: powerToggleBtn.width
                height: powerToggleBtn.height
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
                            powerToggleScope.forceActiveFocus()
                            if (root.powered) btPowerOff.running = true
                            else btPowerOn.running = true
                        }
                    }
                }

                Keys.onReturnPressed: {
                    if (root.powered) btPowerOff.running = true
                    else btPowerOn.running = true
                }
            }

            FocusScope {
                id: scanScope
                width: scanBtn.width
                height: scanBtn.height
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
                            scanScope.forceActiveFocus()
                            if (!root.scanning) btScanOn.running = true
                        }
                    }
                }

                Keys.onReturnPressed: {
                    if (!root.scanning) btScanOn.running = true
                }
            }

            Item { Layout.fillWidth: true }

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
                color: pairedList.currentIndex === index && pairedList.activeFocus
                       ? Theme.surfaceHover : Theme.surface
                border.width: 2
                border.color: modelData.connected ? Theme.online : Theme.surfaceBorder

                Behavior on color { ColorAnimation { duration: 150 } }

                RowLayout {
                    anchors.fill: parent
                    anchors.leftMargin: 24
                    anchors.rightMargin: 24
                    anchors.topMargin: 16
                    anchors.bottomMargin: 16
                    spacing: 16

                    Rectangle {
                        width: 20; height: 20; radius: 10
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
                        pairedList.currentIndex = index
                        pairedList.forceActiveFocus()
                    }
                    onDoubleClicked: {
                        if (modelData.connected) {
                            btDisconnect.mac = modelData.mac
                            btDisconnect.running = true
                        } else {
                            btConnect.mac = modelData.mac
                            btConnect.running = true
                        }
                    }
                }
            }

            Keys.onReturnPressed: {
                if (currentIndex >= 0 && currentIndex < root.pairedDevices.length) {
                    let dev = root.pairedDevices[currentIndex]
                    if (dev.connected) {
                        btDisconnect.mac = dev.mac
                        btDisconnect.running = true
                    } else {
                        btConnect.mac = dev.mac
                        btConnect.running = true
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
                color: availList.currentIndex === index && availList.activeFocus
                       ? Theme.surfaceHover : Theme.surface
                border.width: 2
                border.color: Theme.surfaceBorder

                Behavior on color { ColorAnimation { duration: 150 } }

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
                        availList.currentIndex = index
                        availList.forceActiveFocus()
                    }
                    onDoubleClicked: {
                        btPair.mac = modelData.mac
                        btPair.running = true
                    }
                }
            }

            Keys.onReturnPressed: {
                if (currentIndex >= 0 && currentIndex < root.availableDevices.length) {
                    btPair.mac = root.availableDevices[currentIndex].mac
                    btPair.running = true
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
