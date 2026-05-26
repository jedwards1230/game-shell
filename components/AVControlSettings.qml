import QtQuick
import QtQuick.Layouts
import Quickshell.Io

Item {
    id: root

    property bool cecAvailable: false
    property bool hasAvScript: false
    // "cec-ctl" or "cec-client" — whichever was found
    property string cecTool: ""
    // Resolved path to living-room-cec (empty if not found)
    property string avScriptPath: ""
    property var devices: []
    property string statusText: "Checking CEC availability..."
    property string actionFeedback: ""

    // --- Tool detection: living-room-cec > cec-ctl > cec-client ---
    // Checks command -v AND explicit paths to handle PATH gaps
    Process {
        id: detectTools
        command: ["bash", "-c", "if [ -x /usr/local/bin/living-room-cec ]; then echo script:/usr/local/bin/living-room-cec; elif [ -x /usr/local/sbin/living-room-cec ]; then echo script:/usr/local/sbin/living-room-cec; elif command -v living-room-cec >/dev/null 2>&1; then echo script:$(command -v living-room-cec); fi; command -v cec-ctl >/dev/null 2>&1 && echo tool:cec-ctl; command -v cec-client >/dev/null 2>&1 && echo tool:cec-client; echo done"]
        stdout: SplitParser {
            onRead: (line) => {
                var trimmed = line.trim()
                if (trimmed.startsWith("script:")) {
                    root.hasAvScript = true
                    root.avScriptPath = trimmed.substring(7)
                } else if (trimmed.startsWith("tool:") && root.cecTool === "") {
                    // First tool found wins (cec-ctl before cec-client)
                    root.cecTool = trimmed.substring(5)
                    root.cecAvailable = true
                } else if (trimmed === "done") {
                    // If we have a script but no low-level tool, still mark available
                    if (root.hasAvScript && !root.cecAvailable) {
                        root.cecAvailable = true
                    }
                    if (root.cecAvailable) {
                        root.statusText = "CEC available — scanning devices..."
                        root.startScan()
                    } else {
                        root.statusText = "CEC tools not installed"
                    }
                }
            }
        }
    }

    // --- Device scan ---
    // Probes well-known CEC logical addresses using whatever tool is available.
    // cec-ctl: query power, OSD name, vendor per address
    // cec-client: legacy scan command
    Process {
        id: scanDevices
        command: ["bash", "-c", "true"]
        stdout: SplitParser {
            property string buffer: ""
            onRead: (line) => { buffer += line + "\n" }
        }
        onExited: {
            root._parseScanOutput(scanDevices.stdout.buffer)
            scanDevices.stdout.buffer = ""
        }
    }

    function startScan() {
        scanDevices.command = ["bash", "-c", root._buildScanCommand()]
        scanDevices.running = true
    }

    // --- CEC commands ---
    Process {
        id: wakeCmd
        command: ["bash", "-c", ""]
        onExited: (exitCode) => {
            root.actionFeedback = exitCode === 0 ? "Wake command sent" : "Wake command failed"
            feedbackTimer.restart()
            if (exitCode === 0) refreshAfterAction.restart()
        }
    }

    Process {
        id: sleepCmd
        command: ["bash", "-c", ""]
        onExited: (exitCode) => {
            root.actionFeedback = exitCode === 0 ? "Sleep command sent" : "Sleep command failed"
            feedbackTimer.restart()
            if (exitCode === 0) refreshAfterAction.restart()
        }
    }

    Process {
        id: switchInputCmd
        command: ["bash", "-c", ""]
        onExited: (exitCode) => {
            root.actionFeedback = exitCode === 0 ? "Input switch command sent" : "Input switch command failed"
            feedbackTimer.restart()
        }
    }

    // --- Timers ---
    Timer {
        id: autoRefresh
        interval: 30000
        running: root.visible && root.cecAvailable
        repeat: true
        onTriggered: { root.startScan() }
    }

    Timer {
        id: feedbackTimer
        interval: 3000
        onTriggered: { root.actionFeedback = "" }
    }

    Timer {
        id: refreshAfterAction
        interval: 5000
        onTriggered: { root.startScan() }
    }

    Component.onCompleted: {
        detectTools.running = true
    }

    onVisibleChanged: {
        if (visible) {
            detectTools.running = true
            if (root.cecAvailable) {
                wakeScope.forceActiveFocus()
            } else {
                root.forceActiveFocus()
            }
        }
    }

    // --- Build scan command based on available tool ---
    function _buildScanCommand() {
        // Prefer living-room-cec status — it's the high-level wrapper that works reliably
        if (root.hasAvScript) {
            return root.avScriptPath + " status 2>/dev/null"
        }
        if (root.cecTool === "cec-ctl") {
            // Probe TV (0) and Audio System (5) — the standard CEC addresses
            // For each: query power status, OSD name, vendor ID
            return [
                "for addr in 0 5; do",
                "  echo \"===DEVICE addr=$addr===\"",
                "  timeout 3 cec-ctl --to $addr --give-device-power-status 2>/dev/null",
                "  timeout 3 cec-ctl --to $addr --give-osd-name 2>/dev/null",
                "  timeout 3 cec-ctl --to $addr --give-device-vendor-id 2>/dev/null",
                "done"
            ].join("; ")
        }
        if (root.cecTool === "cec-client") {
            return "echo 'scan' | timeout 10 cec-client -s -d 1 2>/dev/null"
        }
        // No tools available
        return "echo 'no-tool'"
    }

    // --- Parse scan output based on tool ---
    function _parseScanOutput(output) {
        if (root.hasAvScript) {
            _parseScriptStatusOutput(output)
        } else if (root.cecTool === "cec-ctl") {
            _parseCecCtlOutput(output)
        } else if (root.cecTool === "cec-client") {
            _parseCecClientOutput(output)
        }
    }

    // --- Parse cec-ctl output ---
    function _parseCecCtlOutput(output) {
        var devs = []
        var sections = output.split("===DEVICE")
        for (var i = 1; i < sections.length; i++) {
            var section = sections[i]

            // Extract logical address from header
            var addrMatch = section.match(/addr=(\d+)===/)
            if (!addrMatch) continue

            var logAddr = parseInt(addrMatch[1])
            var dev = {
                logicalAddress: logAddr,
                name: logAddr === 0 ? "TV" : logAddr === 5 ? "Audio System" : "Device " + logAddr,
                physicalAddress: "",
                vendor: "",
                powerStatus: "unknown",
                type: logAddr === 0 ? "TV" : logAddr === 5 ? "Audio System" : ""
            }

            // Power status: "pwr-state: on (0x00)" or "pwr-state: standby (0x01)"
            var pwrMatch = section.match(/pwr-state:\s*(\S+)/)
            if (pwrMatch) {
                dev.powerStatus = pwrMatch[1].toLowerCase()
            } else {
                // No response = device not present on bus
                continue
            }

            // OSD name: "name: OLED65C2PU"
            var nameMatch = section.match(/^\s*name:\s*(.+)$/m)
            if (nameMatch) {
                dev.name = nameMatch[1].trim()
            }

            // Vendor: "vendor-id: 0x00e091 (LG)"
            var vendorMatch = section.match(/vendor-id:\s*\S+\s*\((\S+)\)/)
            if (vendorMatch) {
                dev.vendor = vendorMatch[1]
            }

            devs.push(dev)
        }

        root.devices = devs
        if (devs.length > 0) {
            root.statusText = devs.length + " device" + (devs.length !== 1 ? "s" : "") + " detected"
        } else {
            root.statusText = "No CEC devices found"
        }
    }

    // --- Parse cec-client scan output ---
    function _parseCecClientOutput(output) {
        var devs = []
        var lines = output.split("\n")
        var current = null

        for (var i = 0; i < lines.length; i++) {
            var line = lines[i]

            var deviceMatch = line.match(/device\s+#(\d+):/)
            if (deviceMatch) {
                if (current) devs.push(current)
                current = {
                    logicalAddress: parseInt(deviceMatch[1]),
                    name: "Unknown",
                    physicalAddress: "",
                    vendor: "",
                    powerStatus: "unknown",
                    type: ""
                }
                continue
            }
            if (!current) continue

            var osdMatch = line.match(/osd string\s*:\s*(.+)/i)
            if (osdMatch) { current.name = osdMatch[1].trim(); continue }

            var physMatch = line.match(/address\s*:\s*([0-9.]+)/i)
            if (physMatch) { current.physicalAddress = physMatch[1].trim(); continue }

            var vendorMatch = line.match(/vendor\s*:\s*(.+)/i)
            if (vendorMatch) { current.vendor = vendorMatch[1].trim(); continue }

            var powerMatch = line.match(/power status\s*:\s*(.+)/i)
            if (powerMatch) { current.powerStatus = powerMatch[1].trim().toLowerCase(); continue }

            var typeMatch = line.match(/type\s*:\s*(.+)/i)
            if (typeMatch) { current.type = typeMatch[1].trim(); continue }
        }
        if (current) devs.push(current)

        root.devices = devs
        if (devs.length > 0) {
            root.statusText = devs.length + " device" + (devs.length !== 1 ? "s" : "") + " detected"
        } else {
            root.statusText = "No CEC devices found"
        }
    }

    // --- Parse living-room-cec status output ---
    // Output format: "  TV: on" / "  AVR: standby"
    function _parseScriptStatusOutput(output) {
        var devs = []
        var lines = output.split("\n")

        for (var i = 0; i < lines.length; i++) {
            var match = lines[i].match(/^\s*(TV|AVR)\s*:\s*(\S+)/i)
            if (match) {
                devs.push({
                    logicalAddress: match[1].toUpperCase() === "TV" ? 0 : 5,
                    name: match[1].toUpperCase(),
                    physicalAddress: "",
                    vendor: "",
                    powerStatus: match[2].toLowerCase(),
                    type: match[1].toUpperCase() === "TV" ? "TV" : "Audio System"
                })
            }
        }

        root.devices = devs
        if (devs.length > 0) {
            root.statusText = devs.length + " device" + (devs.length !== 1 ? "s" : "") + " detected"
        } else {
            root.statusText = "No CEC devices found"
        }
    }

    function doWake() {
        if (root.hasAvScript) {
            wakeCmd.command = [root.avScriptPath, "on"]
        } else if (root.cecTool === "cec-ctl") {
            wakeCmd.command = ["bash", "-c", "cec-ctl --to 0 --image-view-on 2>/dev/null"]
        } else {
            wakeCmd.command = ["bash", "-c", "echo 'on 0' | cec-client -s -d 1 2>/dev/null"]
        }
        wakeCmd.running = true
    }

    function doSleep() {
        if (root.hasAvScript) {
            sleepCmd.command = [root.avScriptPath, "off"]
        } else if (root.cecTool === "cec-ctl") {
            sleepCmd.command = ["bash", "-c", "cec-ctl --to 0 --standby 2>/dev/null; cec-ctl --to 15 --standby 2>/dev/null"]
        } else {
            sleepCmd.command = ["bash", "-c", "echo 'standby 0' | cec-client -s -d 1 2>/dev/null"]
        }
        sleepCmd.running = true
    }

    function doSwitchInput() {
        if (root.hasAvScript) {
            // living-room-cec "on" includes input switching
            switchInputCmd.command = [root.avScriptPath, "on"]
        } else if (root.cecTool === "cec-ctl") {
            switchInputCmd.command = ["bash", "-c", "cec-ctl --active-source phys-addr=$(cec-ctl -s 2>/dev/null | grep -oP 'Physical Address\\s*:\\s*\\K[0-9.]+') 2>/dev/null"]
        } else if (root.cecTool === "cec-client") {
            switchInputCmd.command = ["bash", "-c", "echo 'as' | cec-client -s -d 1 2>/dev/null"]
        } else {
            switchInputCmd.command = ["bash", "-c", "echo 'no CEC tool available'; exit 1"]
        }
        switchInputCmd.running = true
    }

    // --- UI ---
    ColumnLayout {
        anchors.fill: parent
        anchors.margins: Theme.padding
        spacing: 32

        // Header with status
        RowLayout {
            Layout.fillWidth: true
            spacing: 24

            Text {
                text: "AV Control"
                font.pixelSize: Theme.fontBody
                font.bold: true
                color: Theme.textPrimary
            }

            Item { Layout.fillWidth: true }

            // Status indicator
            Rectangle {
                width: statusRow.implicitWidth + 32
                height: 56
                radius: 28
                color: root.cecAvailable ? Qt.rgba(0.176, 0.541, 0.306, 0.2) : Theme.surfaceHover

                RowLayout {
                    id: statusRow
                    anchors.centerIn: parent
                    spacing: 12

                    Rectangle {
                        width: 16
                        height: 16
                        radius: 8
                        color: root.cecAvailable ? Theme.online : Theme.textMuted
                    }

                    Text {
                        text: root.statusText
                        font.pixelSize: Theme.fontSmall
                        color: root.cecAvailable ? Theme.online : Theme.textSecondary
                    }
                }
            }

            // Refresh button
            FocusScope {
                id: refreshScope
                width: refreshBtn.implicitWidth
                height: refreshBtn.implicitHeight
                visible: root.cecAvailable

                KeyNavigation.down: wakeScope

                SettingsButton {
                    id: refreshBtn
                    text: "Refresh"
                    focus: parent.activeFocus
                    anchors.fill: parent
                }

                Keys.onReturnPressed: {
                    root.statusText = "Scanning..."
                    root.startScan()
                }

                MouseArea {
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: {
                        refreshScope.forceActiveFocus()
                        root.statusText = "Scanning..."
                        root.startScan()
                    }
                }
            }
        }

        // CEC unavailable message
        Rectangle {
            Layout.fillWidth: true
            Layout.preferredHeight: 200
            radius: 24
            color: Theme.surface
            border.width: 2
            border.color: Theme.surfaceBorder
            visible: !root.cecAvailable

            ColumnLayout {
                anchors.centerIn: parent
                spacing: 16

                Text {
                    text: "HDMI-CEC Not Available"
                    font.pixelSize: Theme.fontTitle
                    font.bold: true
                    color: Theme.textPrimary
                    Layout.alignment: Qt.AlignHCenter
                }

                Text {
                    text: "Install v4l-utils (cec-ctl) or libcec (cec-client) for HDMI-CEC device control."
                    font.pixelSize: Theme.fontSmall
                    color: Theme.textSecondary
                    Layout.alignment: Qt.AlignHCenter
                }
            }
        }

        // Quick actions — always visible when CEC available
        ColumnLayout {
            Layout.fillWidth: true
            spacing: 16
            visible: root.cecAvailable

            Text {
                text: "Quick Actions"
                font.pixelSize: Theme.fontBody
                font.bold: true
                color: Theme.textPrimary
            }

            // Feedback text
            Text {
                text: root.actionFeedback
                font.pixelSize: Theme.fontSmall
                color: Theme.online
                visible: root.actionFeedback !== ""
                Layout.alignment: Qt.AlignHCenter
            }

            RowLayout {
                Layout.alignment: Qt.AlignHCenter
                spacing: 32

                // Wake AV
                FocusScope {
                    id: wakeScope
                    width: 340
                    height: 120
                    focus: true
                    activeFocusOnTab: true

                    KeyNavigation.up: refreshScope
                    KeyNavigation.right: sleepScope
                    KeyNavigation.down: deviceListView.count > 0 ? deviceListView : wakeScope

                    Rectangle {
                        anchors.fill: parent
                        radius: 24
                        color: parent.activeFocus ? Theme.online : Theme.surface
                        border.width: parent.activeFocus ? 0 : 2
                        border.color: Theme.surfaceBorder

                        Behavior on color { ColorAnimation { duration: 150 } }

                        ColumnLayout {
                            anchors.centerIn: parent
                            spacing: 4

                            Text {
                                text: "Wake AV"
                                font.pixelSize: Theme.fontTitle
                                font.bold: true
                                color: wakeScope.activeFocus ? Theme.textOnDark : Theme.textPrimary
                                Layout.alignment: Qt.AlignHCenter
                            }

                            Text {
                                text: root.hasAvScript ? "Using AV script" : "CEC power on"
                                font.pixelSize: Theme.fontSmall
                                color: wakeScope.activeFocus ? Theme.textOnDarkMuted : Theme.textSecondary
                                Layout.alignment: Qt.AlignHCenter
                            }
                        }

                        MouseArea {
                            anchors.fill: parent
                            hoverEnabled: true
                            cursorShape: Qt.PointingHandCursor
                            onClicked: {
                                wakeScope.forceActiveFocus()
                                root.doWake()
                            }
                        }
                    }

                    Keys.onReturnPressed: { root.doWake() }
                }

                // Sleep AV
                FocusScope {
                    id: sleepScope
                    width: 340
                    height: 120
                    activeFocusOnTab: true

                    KeyNavigation.left: wakeScope
                    KeyNavigation.right: switchScope
                    KeyNavigation.down: deviceListView.count > 0 ? deviceListView : sleepScope

                    Rectangle {
                        anchors.fill: parent
                        radius: 24
                        color: parent.activeFocus ? Theme.gold : Theme.surface
                        border.width: parent.activeFocus ? 0 : 2
                        border.color: Theme.surfaceBorder

                        Behavior on color { ColorAnimation { duration: 150 } }

                        ColumnLayout {
                            anchors.centerIn: parent
                            spacing: 4

                            Text {
                                text: "Sleep AV"
                                font.pixelSize: Theme.fontTitle
                                font.bold: true
                                color: sleepScope.activeFocus ? Theme.textOnDark : Theme.textPrimary
                                Layout.alignment: Qt.AlignHCenter
                            }

                            Text {
                                text: root.hasAvScript ? "Using AV script" : "CEC standby"
                                font.pixelSize: Theme.fontSmall
                                color: sleepScope.activeFocus ? Theme.textOnDarkMuted : Theme.textSecondary
                                Layout.alignment: Qt.AlignHCenter
                            }
                        }

                        MouseArea {
                            anchors.fill: parent
                            hoverEnabled: true
                            cursorShape: Qt.PointingHandCursor
                            onClicked: {
                                sleepScope.forceActiveFocus()
                                root.doSleep()
                            }
                        }
                    }

                    Keys.onReturnPressed: { root.doSleep() }
                }

                // Switch Input
                FocusScope {
                    id: switchScope
                    width: 340
                    height: 120
                    activeFocusOnTab: true

                    KeyNavigation.left: sleepScope
                    KeyNavigation.down: deviceListView.count > 0 ? deviceListView : switchScope

                    Rectangle {
                        anchors.fill: parent
                        radius: 24
                        color: parent.activeFocus ? Theme.ember : Theme.surface
                        border.width: parent.activeFocus ? 0 : 2
                        border.color: Theme.surfaceBorder

                        Behavior on color { ColorAnimation { duration: 150 } }

                        ColumnLayout {
                            anchors.centerIn: parent
                            spacing: 4

                            Text {
                                text: "Switch Input"
                                font.pixelSize: Theme.fontTitle
                                font.bold: true
                                color: switchScope.activeFocus ? Theme.textOnDark : Theme.textPrimary
                                Layout.alignment: Qt.AlignHCenter
                            }

                            Text {
                                text: "Set active source"
                                font.pixelSize: Theme.fontSmall
                                color: switchScope.activeFocus ? Theme.textOnDarkMuted : Theme.textSecondary
                                Layout.alignment: Qt.AlignHCenter
                            }
                        }

                        MouseArea {
                            anchors.fill: parent
                            hoverEnabled: true
                            cursorShape: Qt.PointingHandCursor
                            onClicked: {
                                switchScope.forceActiveFocus()
                                root.doSwitchInput()
                            }
                        }
                    }

                    Keys.onReturnPressed: { root.doSwitchInput() }
                }
            }
        }

        // Device list
        ColumnLayout {
            Layout.fillWidth: true
            Layout.fillHeight: true
            spacing: 16
            visible: root.cecAvailable

            Text {
                text: "Detected Devices"
                font.pixelSize: Theme.fontBody
                font.bold: true
                color: Theme.textPrimary
            }

            // No devices placeholder
            Rectangle {
                Layout.fillWidth: true
                Layout.preferredHeight: 120
                radius: 16
                color: Theme.surface
                border.width: 2
                border.color: Theme.surfaceBorder
                visible: root.devices.length === 0

                Text {
                    anchors.centerIn: parent
                    text: "No CEC devices detected. Try refreshing."
                    font.pixelSize: Theme.fontSmall
                    color: Theme.textSecondary
                }
            }

            ListView {
                id: deviceListView
                Layout.fillWidth: true
                Layout.fillHeight: true
                spacing: 16
                clip: true
                model: root.devices
                visible: root.devices.length > 0

                KeyNavigation.up: wakeScope

                delegate: Rectangle {
                    required property int index
                    required property var modelData
                    width: deviceListView.width
                    height: 160
                    radius: 16
                    color: deviceListView.currentIndex === index && deviceListView.activeFocus
                           ? Theme.surfaceHover : Theme.surface
                    border.width: 2
                    border.color: Theme.surfaceBorder

                    Behavior on color { ColorAnimation { duration: 150 } }

                    RowLayout {
                        anchors.fill: parent
                        anchors.leftMargin: 32
                        anchors.rightMargin: 32
                        spacing: 24

                        // Device icon/name
                        ColumnLayout {
                            spacing: 8
                            Layout.fillWidth: true

                            RowLayout {
                                spacing: 16

                                Text {
                                    text: modelData.name
                                    font.pixelSize: Theme.fontBody
                                    font.bold: true
                                    color: Theme.textPrimary
                                }

                                // Power status badge
                                Rectangle {
                                    width: powerLabel.implicitWidth + 24
                                    height: 40
                                    radius: 20
                                    color: {
                                        if (modelData.powerStatus === "on") return Qt.rgba(0.176, 0.541, 0.306, 0.2)
                                        if (modelData.powerStatus === "standby") return Qt.rgba(0.843, 0.651, 0.294, 0.2)
                                        return Theme.surfaceHover
                                    }

                                    Text {
                                        id: powerLabel
                                        anchors.centerIn: parent
                                        text: {
                                            if (modelData.powerStatus === "on") return "On"
                                            if (modelData.powerStatus === "standby") return "Standby"
                                            return "Unknown"
                                        }
                                        font.pixelSize: Theme.fontSmall - 4
                                        color: {
                                            if (modelData.powerStatus === "on") return Theme.online
                                            if (modelData.powerStatus === "standby") return Theme.gold
                                            return Theme.textSecondary
                                        }
                                    }
                                }
                            }

                            RowLayout {
                                spacing: 32

                                Text {
                                    text: "Address: " + modelData.logicalAddress + (modelData.physicalAddress ? " (" + modelData.physicalAddress + ")" : "")
                                    font.pixelSize: Theme.fontSmall
                                    color: Theme.textSecondary
                                }

                                Text {
                                    text: "Vendor: " + (modelData.vendor || "Unknown")
                                    font.pixelSize: Theme.fontSmall
                                    color: Theme.textSecondary
                                    visible: modelData.vendor !== ""
                                }

                                Text {
                                    text: "Type: " + modelData.type
                                    font.pixelSize: Theme.fontSmall
                                    color: Theme.textSecondary
                                    visible: modelData.type !== ""
                                }
                            }
                        }
                    }

                    MouseArea {
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: {
                            deviceListView.currentIndex = index
                            deviceListView.forceActiveFocus()
                        }
                    }
                }
            }
        }

        // Hint bar
        Text {
            text: root.cecAvailable
                  ? "A: Select  |  Auto-refresh every 30s" + (root.cecTool ? "  |  " + root.cecTool : "")
                  : "Install v4l-utils or libcec for CEC support"
            font.pixelSize: Theme.fontHint
            color: Theme.textSecondary
            Layout.alignment: Qt.AlignHCenter
        }
    }
}
