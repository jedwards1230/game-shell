import QtQuick
import QtQuick.Layouts
import Quickshell.Io

Item {
    id: root

    property var activeConnections: []
    property string ipAddress: ""
    property var wifiNetworks: []
    property bool hasWifi: false

    // --- Processes ---

    Process {
        id: getActiveConns
        command: ["nmcli", "-t", "-f", "NAME,TYPE,DEVICE", "connection", "show", "--active"]
        stdout: SplitParser {
            property var collected: []
            onRead: (line) => {
                let parts = line.trim().split(":")
                if (parts.length >= 3) {
                    collected.push({
                        name: parts[0],
                        type: parts[1],
                        device: parts[2]
                    })
                    if (parts[1] === "802-11-wireless") root.hasWifi = true
                }
            }
        }
        onExited: {
            root.activeConnections = getActiveConns.stdout.collected
            getActiveConns.stdout.collected = []
        }
    }

    Process {
        id: getIP
        command: ["bash", "-c", "ip -4 -o addr show | grep -v '127.0.0.1' | head -3 | awk '{print $2\": \"$4}'"]
        stdout: SplitParser {
            property var lines: []
            onRead: (line) => { lines.push(line.trim()) }
        }
        onExited: {
            root.ipAddress = getIP.stdout.lines.join("\n")
            getIP.stdout.lines = []
        }
    }

    Process {
        id: getWifi
        command: ["nmcli", "-t", "-f", "SSID,SIGNAL,SECURITY,IN-USE", "device", "wifi", "list", "--rescan", "auto"]
        stdout: SplitParser {
            property var collected: []
            onRead: (line) => {
                let parts = line.trim().split(":")
                if (parts.length >= 3 && parts[0] !== "") {
                    collected.push({
                        ssid: parts[0],
                        signal: parseInt(parts[1]) || 0,
                        security: parts[2] || "Open",
                        inUse: parts.length >= 4 && parts[3] === "*"
                    })
                }
            }
        }
        onExited: {
            // Deduplicate by SSID, keep highest signal
            let seen = {}
            let deduped = []
            for (let i = 0; i < getWifi.stdout.collected.length; i++) {
                let net = getWifi.stdout.collected[i]
                if (!seen[net.ssid] || net.signal > seen[net.ssid].signal) {
                    seen[net.ssid] = net
                }
            }
            for (let ssid in seen) deduped.push(seen[ssid])
            // Sort by signal descending
            deduped.sort(function(a, b) { return b.signal - a.signal })
            root.wifiNetworks = deduped
            getWifi.stdout.collected = []
        }
    }

    Process {
        id: checkWifiDevice
        command: ["nmcli", "-t", "-f", "TYPE,DEVICE", "device", "status"]
        stdout: SplitParser {
            onRead: (line) => {
                if (line.indexOf("wifi") >= 0) root.hasWifi = true
            }
        }
        onExited: {
            if (root.hasWifi) getWifi.running = true
        }
    }

    Component.onCompleted: {
        getActiveConns.running = true
        getIP.running = true
        checkWifiDevice.running = true
    }

    onVisibleChanged: {
        if (visible) {
            getActiveConns.running = true
            getIP.running = true
            checkWifiDevice.running = true
        }
    }

    ColumnLayout {
        anchors.fill: parent
        anchors.margins: Theme.padding
        spacing: 32

        // Connection status
        Text {
            text: "Active Connections"
            font.pixelSize: Theme.fontBody
            font.bold: true
            color: Theme.textPrimary
        }

        ListView {
            Layout.fillWidth: true
            Layout.preferredHeight: Math.min(contentHeight, 300)
            spacing: 8
            clip: true
            model: root.activeConnections
            interactive: false

            delegate: Rectangle {
                required property var modelData
                width: parent ? parent.width : 0
                height: 80
                radius: 16
                color: Theme.surface
                border.width: 2
                border.color: Theme.surfaceBorder

                RowLayout {
                    anchors.fill: parent
                    anchors.margins: 16
                    spacing: 16

                    Rectangle {
                        width: 20; height: 20; radius: 10
                        color: Theme.online
                    }

                    Text {
                        text: modelData.name
                        font.pixelSize: Theme.fontSmall
                        font.bold: true
                        color: Theme.textPrimary
                        Layout.fillWidth: true
                    }

                    Text {
                        text: modelData.type === "802-3-ethernet" ? "Ethernet" :
                              modelData.type === "802-11-wireless" ? "WiFi" :
                              modelData.type
                        font.pixelSize: Theme.fontSmall
                        color: Theme.textSecondary
                    }

                    Text {
                        text: modelData.device
                        font.pixelSize: Theme.fontSmall
                        color: Theme.textSecondary
                    }
                }
            }
        }

        Text {
            text: root.activeConnections.length === 0 ? "No active connections" : ""
            font.pixelSize: Theme.fontSmall
            color: Theme.textSecondary
            visible: text !== ""
        }

        // IP Address
        Text {
            text: "IP Addresses"
            font.pixelSize: Theme.fontBody
            font.bold: true
            color: Theme.textPrimary
        }

        Rectangle {
            Layout.fillWidth: true
            height: ipLabel.implicitHeight + 32
            radius: 16
            color: Theme.surface

            Text {
                id: ipLabel
                anchors.fill: parent
                anchors.margins: 16
                text: root.ipAddress || "Fetching..."
                font.pixelSize: Theme.fontSmall
                font.family: "monospace"
                color: Theme.textPrimary
                wrapMode: Text.Wrap
            }
        }

        // WiFi networks
        Text {
            text: "WiFi Networks"
            font.pixelSize: Theme.fontBody
            font.bold: true
            color: Theme.textPrimary
            visible: root.hasWifi
        }

        ListView {
            id: wifiList
            Layout.fillWidth: true
            Layout.fillHeight: true
            spacing: 8
            clip: true
            visible: root.hasWifi
            model: root.wifiNetworks
            focus: root.hasWifi

            delegate: Rectangle {
                required property int index
                required property var modelData
                width: wifiList.width
                height: 80
                radius: 16
                color: {
                    if (modelData.inUse) return Theme.sidebarActive
                    if (wifiList.currentIndex === index && wifiList.activeFocus) return Theme.surfaceHover
                    return Theme.surface
                }
                border.width: 2
                border.color: modelData.inUse ? Theme.focusBorder : Theme.surfaceBorder

                Behavior on color { ColorAnimation { duration: 150 } }

                RowLayout {
                    anchors.fill: parent
                    anchors.margins: 16
                    spacing: 16

                    Text {
                        text: modelData.ssid
                        font.pixelSize: Theme.fontSmall
                        font.bold: modelData.inUse
                        color: modelData.inUse ? Theme.textOnDark : Theme.textPrimary
                        Layout.fillWidth: true
                    }

                    // Signal strength bar
                    Row {
                        spacing: 4
                        Repeater {
                            model: 4
                            Rectangle {
                                required property int index
                                width: 8
                                height: 12 + index * 8
                                radius: 2
                                color: {
                                    let threshold = (index + 1) * 25
                                    let active = modelData.signal >= threshold
                                    if (modelData.inUse)
                                        return active ? Theme.textOnDark : Theme.textOnDarkMuted
                                    return active ? Theme.focusBorder : Theme.surfaceHover
                                }
                                anchors.bottom: parent.bottom
                            }
                        }
                    }

                    Text {
                        text: modelData.signal + "%"
                        font.pixelSize: Theme.fontSmall
                        color: modelData.inUse ? Theme.textOnDarkMuted : Theme.textSecondary
                    }

                    Text {
                        text: modelData.security
                        font.pixelSize: Theme.fontSmall
                        color: modelData.inUse ? Theme.textOnDarkMuted : Theme.textSecondary
                    }

                    Text {
                        text: modelData.inUse ? "Connected" : ""
                        font.pixelSize: Theme.fontSmall
                        font.bold: true
                        color: Theme.textOnDark
                        visible: modelData.inUse
                    }
                }

                MouseArea {
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: {
                        wifiList.currentIndex = index
                        wifiList.forceActiveFocus()
                    }
                }
            }
        }

        Text {
            text: !root.hasWifi ? "No WiFi adapter detected" :
                  root.wifiNetworks.length === 0 ? "No WiFi networks found" : ""
            font.pixelSize: Theme.fontSmall
            color: Theme.textSecondary
            visible: text !== ""
        }

        // Info hint
        Text {
            text: "Network configuration is read-only"
            font.pixelSize: Theme.fontHint
            color: Theme.textSecondary
            Layout.alignment: Qt.AlignHCenter
        }
    }
}
