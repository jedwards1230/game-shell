import QtQuick
import QtQuick.Layouts
import Quickshell.Io

FocusScope {
    id: root

    property var servers: []
    property bool showAddForm: false
    property int confirmRemoveIndex: -1

    // Connection status per host: { "host": "paired" | "unpaired" | "offline" | "checking" }
    property var hostStatus: ({})
    property int _statusCheckIndex: -1

    // Pairing state
    property int pairingServerIndex: -1
    property string pairingPin: ""

    // Form fields
    property string newName: ""
    property string newHost: ""
    property string newApp: "Desktop"
    property string newResolution: "3840x2160"
    property int newFps: 120
    property bool newHdr: true
    property string newCodec: "HEVC"

    // --- Processes ---

    Process {
        id: loadServers
        command: ["cat", "/opt/game-shell/targets.json"]
        stdout: SplitParser {
            onRead: line => {
                try {
                    root.servers = JSON.parse(line);
                } catch (e) {
                    root.servers = [];
                }
            }
        }
        onExited: root._checkAllStatuses()
    }

    Process {
        id: saveServers
        property string json: "[]"
        command: ["bash", "-c", "echo '" + json + "' > /opt/game-shell/targets.json"]
        // Refresh the provider's targets so the home screen reflects edits.
        onExited: StreamProviders.active.loadTargets()
    }

    // Connection status checker — runs moonlight list per host
    Process {
        id: statusChecker
        property string _host: ""
        property string _output: ""
        command: ["moonlight", "list", _host]
        stdout: SplitParser {
            onRead: line => {
                statusChecker._output += line + "\n";
            }
        }
        stderr: SplitParser {
            onRead: line => {
                statusChecker._output += line + "\n";
            }
        }
        onExited: (exitCode, exitStatus) => {
            let host = statusChecker._host;
            let output = statusChecker._output;
            statusChecker._output = "";

            let updated = root.hostStatus;
            if (output.indexOf("not been paired") >= 0)
                updated[host] = "unpaired";
            else if (exitCode === 0)
                updated[host] = "paired";
            else
                updated[host] = "offline";
            root.hostStatus = JSON.parse(JSON.stringify(updated));

            root._statusCheckIndex++;
            root._checkNextStatus();
        }
    }

    // Pairing process
    Process {
        id: pairProcess
        property string _output: ""
        stdout: SplitParser {
            onRead: line => {
                pairProcess._output += line + "\n";
                let match = line.match(/(\d{4})/);
                if (match && root.pairingPin === "")
                    root.pairingPin = match[1];
            }
        }
        stderr: SplitParser {
            onRead: line => {
                pairProcess._output += line + "\n";
                let match = line.match(/(\d{4})/);
                if (match && root.pairingPin === "")
                    root.pairingPin = match[1];
            }
        }
        onExited: (exitCode, exitStatus) => {
            let host = root.pairingServerIndex >= 0 && root.pairingServerIndex < root.servers.length ? root.servers[root.pairingServerIndex].host : "";
            if (exitCode === 0 && host !== "") {
                let updated = root.hostStatus;
                updated[host] = "paired";
                root.hostStatus = JSON.parse(JSON.stringify(updated));
                NotificationManager.info("moonlight", "Pairing Successful", host);
            } else {
                NotificationManager.warn("moonlight", "Pairing Failed", pairProcess._output.substring(0, 100));
            }
            pairProcess._output = "";
            root.pairingServerIndex = -1;
            root.pairingPin = "";
            serverList.forceActiveFocus();
        }
    }

    function _checkAllStatuses() {
        if (root.servers.length === 0)
            return;
        _statusCheckIndex = 0;
        let initial = {};
        for (let i = 0; i < root.servers.length; i++)
            initial[root.servers[i].host] = "checking";
        root.hostStatus = initial;
        _checkNextStatus();
    }

    function _checkNextStatus() {
        if (_statusCheckIndex >= root.servers.length) {
            _statusCheckIndex = -1;
            return;
        }
        let host = root.servers[_statusCheckIndex].host || "";
        if (host === "") {
            _statusCheckIndex++;
            _checkNextStatus();
            return;
        }
        statusChecker._host = host;
        statusChecker._output = "";
        statusChecker.running = true;
    }

    function _statusColor(host) {
        let s = root.hostStatus[host] || "";
        if (s === "paired")
            return Theme.online;
        if (s === "unpaired")
            return Theme.warning;
        if (s === "offline")
            return Theme.offline;
        return Theme.textMuted;
    }

    function _statusText(host) {
        let s = root.hostStatus[host] || "";
        if (s === "paired")
            return "Paired";
        if (s === "unpaired")
            return "Not Paired";
        if (s === "offline")
            return "Offline";
        if (s === "checking")
            return "Checking...";
        return "";
    }

    function startPairing(idx) {
        if (idx < 0 || idx >= root.servers.length)
            return;
        root.pairingServerIndex = idx;
        root.pairingPin = "";
        pairProcess._output = "";
        pairProcess.command = ["moonlight", "pair", root.servers[idx].host];
        pairProcess.running = true;
    }

    function cancelPairing() {
        pairProcess.running = false;
        pairProcess._output = "";
        root.pairingServerIndex = -1;
        root.pairingPin = "";
        serverList.forceActiveFocus();
    }

    function persistServers() {
        saveServers.json = JSON.stringify(root.servers);
        saveServers.running = true;
    }

    function addServer() {
        if (newName === "" || newHost === "")
            return;
        let entry = {
            name: newName,
            host: newHost,
            app: newApp,
            resolution: newResolution,
            fps: newFps,
            hdr: newHdr,
            codec: newCodec
        };
        let list = root.servers.slice();
        list.push(entry);
        root.servers = list;
        persistServers();
        resetForm();
    }

    function removeServer(idx) {
        let list = root.servers.slice();
        list.splice(idx, 1);
        root.servers = list;
        persistServers();
        root.confirmRemoveIndex = -1;
    }

    function resetForm() {
        newName = "";
        newHost = "";
        newApp = "Desktop";
        newResolution = "3840x2160";
        newFps = 120;
        newHdr = true;
        newCodec = "HEVC";
        showAddForm = false;
    }

    Component.onCompleted: {
        loadServers.running = true;
    }

    onVisibleChanged: {
        if (visible) {
            loadServers.running = true;
            viewModeRow.forceActiveFocus();
        }
    }

    ColumnLayout {
        anchors.fill: parent
        anchors.margins: Theme.padding
        spacing: 24

        // === Display Mode Toggle ===
        Text {
            text: "Display Mode"
            font.pixelSize: Theme.fontBody
            font.bold: true
            color: Theme.textPrimary
        }

        RowLayout {
            id: viewModeRow
            Layout.alignment: Qt.AlignLeft
            spacing: 24

            property int focusIndex: Theme.moonlightViewMode === "apps" ? 1 : 0

            Keys.onLeftPressed: {
                if (focusIndex > 0)
                    focusIndex--;
            }
            Keys.onRightPressed: {
                if (focusIndex < 1)
                    focusIndex++;
            }
            Keys.onReturnPressed: {
                Theme.setMoonlightViewMode(focusIndex === 0 ? "servers" : "apps");
            }
            Keys.onDownPressed: {
                if (serverList.count > 0)
                    serverList.forceActiveFocus();
                else
                    addBtnScope.forceActiveFocus();
            }

            Repeater {
                model: [
                    {
                        id: "servers",
                        label: "Servers",
                        desc: "One card per host"
                    },
                    {
                        id: "apps",
                        label: "Apps",
                        desc: "Per-host app rows"
                    }
                ]

                Rectangle {
                    required property var modelData
                    required property int index
                    width: 340
                    height: 180
                    radius: Theme.cardRadius
                    color: Theme.surface
                    border.width: Theme.moonlightViewMode === modelData.id ? 3 : 2
                    border.color: Theme.moonlightViewMode === modelData.id ? Theme.focusBorder : (viewModeRow.focusIndex === index && viewModeRow.activeFocus ? Theme.focusBorder : Theme.surfaceBorder)

                    Behavior on border.color {
                        ColorAnimation {
                            duration: 150
                        }
                    }

                    Rectangle {
                        anchors.fill: parent
                        radius: parent.radius
                        color: Theme.surfaceHover
                        visible: viewModeRow.focusIndex === index && viewModeRow.activeFocus && Theme.moonlightViewMode !== modelData.id
                    }

                    ColumnLayout {
                        anchors.fill: parent
                        anchors.margins: 24
                        spacing: 8

                        Item {
                            Layout.fillHeight: true
                        }

                        Text {
                            text: modelData.label
                            font.pixelSize: Theme.fontBody
                            font.bold: true
                            color: Theme.textPrimary
                            Layout.alignment: Qt.AlignHCenter
                        }

                        Text {
                            text: modelData.desc
                            font.pixelSize: Theme.fontCaption
                            color: Theme.textSecondary
                            Layout.alignment: Qt.AlignHCenter
                        }

                        Item {
                            Layout.fillHeight: true
                        }
                    }

                    MouseArea {
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: {
                            viewModeRow.focusIndex = index;
                            viewModeRow.forceActiveFocus();
                            Theme.setMoonlightViewMode(modelData.id);
                        }
                    }
                }
            }
        }

        // Divider
        Rectangle {
            Layout.fillWidth: true
            height: 2
            color: Theme.surfaceBorder
        }

        Text {
            text: "Streaming Servers"
            font.pixelSize: Theme.fontBody
            font.bold: true
            color: Theme.textPrimary
        }

        // Server list
        ListView {
            id: serverList
            Layout.fillWidth: true
            Layout.fillHeight: !root.showAddForm
            Layout.preferredHeight: root.showAddForm ? Math.min(contentHeight, 400) : -1
            spacing: 16
            clip: true
            model: root.servers
            focus: false

            delegate: Rectangle {
                required property int index
                required property var modelData
                width: serverList.width
                height: 180
                radius: Theme.cardRadius
                color: serverList.currentIndex === index && serverList.activeFocus ? Theme.surfaceHover : Theme.surface
                border.width: 2
                border.color: Theme.surfaceBorder

                Behavior on color {
                    ColorAnimation {
                        duration: 150
                    }
                }

                RowLayout {
                    anchors.fill: parent
                    anchors.leftMargin: 32
                    anchors.rightMargin: 32
                    anchors.topMargin: 24
                    anchors.bottomMargin: 24
                    spacing: 32

                    ColumnLayout {
                        Layout.fillWidth: true
                        spacing: 8

                        RowLayout {
                            spacing: 16

                            Text {
                                text: modelData.name
                                font.pixelSize: Theme.fontBody
                                font.bold: true
                                color: Theme.textPrimary
                            }

                            Text {
                                text: modelData.host
                                font.pixelSize: Theme.fontSmall
                                color: Theme.textSecondary
                            }

                            // Status indicator
                            RowLayout {
                                spacing: 8

                                Rectangle {
                                    width: 16
                                    height: 16
                                    radius: 8
                                    color: root._statusColor(modelData.host)
                                }

                                Text {
                                    text: root._statusText(modelData.host)
                                    font.pixelSize: Theme.fontSmall
                                    color: root._statusColor(modelData.host)
                                }
                            }
                        }

                        RowLayout {
                            spacing: 24

                            Text {
                                text: modelData.app || "Desktop"
                                font.pixelSize: Theme.fontSmall
                                color: Theme.textSecondary
                            }

                            Text {
                                text: (modelData.resolution || "1920x1080") + " @ " + (modelData.fps || 60) + "fps"
                                font.pixelSize: Theme.fontSmall
                                color: Theme.textSecondary
                            }

                            Text {
                                text: (modelData.codec || "H.264") + (modelData.hdr ? " HDR" : "")
                                font.pixelSize: Theme.fontSmall
                                color: Theme.textSecondary
                            }
                        }
                    }

                    // Pair button (visible when unpaired)
                    FocusScope {
                        width: pairBtn.width
                        height: pairBtn.height
                        visible: root.hostStatus[modelData.host] === "unpaired"

                        SettingsButton {
                            id: pairBtn
                            text: "Pair"
                            focus: parent.activeFocus

                            MouseArea {
                                anchors.fill: parent
                                hoverEnabled: true
                                cursorShape: Qt.PointingHandCursor
                                onClicked: root.startPairing(index)
                            }
                        }

                        Keys.onReturnPressed: root.startPairing(index)
                    }

                    // Remove button
                    FocusScope {
                        width: removeBtn.width
                        height: removeBtn.height

                        SettingsButton {
                            id: removeBtn
                            text: "Remove"
                            focus: parent.activeFocus

                            MouseArea {
                                anchors.fill: parent
                                hoverEnabled: true
                                cursorShape: Qt.PointingHandCursor
                                onClicked: {
                                    root.confirmRemoveIndex = index;
                                }
                            }
                        }

                        Keys.onReturnPressed: {
                            root.confirmRemoveIndex = index;
                        }
                    }
                }
            }

            Keys.onReturnPressed:
            // no-op: servers are launched from home screen
            {}

            Keys.onUpPressed: {
                if (currentIndex > 0)
                    currentIndex--;
                else
                    viewModeRow.forceActiveFocus();
            }

            Keys.onDownPressed: {
                if (currentIndex < root.servers.length - 1)
                    currentIndex++;
                else
                    addBtnScope.forceActiveFocus();
            }
        }

        Text {
            text: root.servers.length === 0 ? "No servers configured" : ""
            font.pixelSize: Theme.fontSmall
            color: Theme.textSecondary
            visible: text !== ""
        }

        // Add server button
        FocusScope {
            id: addBtnScope
            width: addBtn.width
            height: addBtn.height
            visible: !root.showAddForm
            activeFocusOnTab: true

            KeyNavigation.up: serverList

            SettingsButton {
                id: addBtn
                text: "Add Server"
                focus: parent.activeFocus

                MouseArea {
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: {
                        addBtnScope.forceActiveFocus();
                        root.showAddForm = true;
                        nameInput.forceActiveFocus();
                    }
                }
            }

            Keys.onReturnPressed: {
                root.showAddForm = true;
                nameInput.forceActiveFocus();
            }
        }

        // Add server form
        Rectangle {
            Layout.fillWidth: true
            Layout.fillHeight: true
            radius: Theme.cardRadius
            color: Theme.surface
            border.width: 2
            border.color: Theme.surfaceBorder
            visible: root.showAddForm

            ColumnLayout {
                anchors.fill: parent
                anchors.margins: 40
                spacing: 24

                Text {
                    text: "New Server"
                    font.pixelSize: Theme.fontBody
                    font.bold: true
                    color: Theme.textPrimary
                }

                // Name
                RowLayout {
                    spacing: 24

                    Text {
                        text: "Name"
                        font.pixelSize: Theme.fontSmall
                        color: Theme.textSecondary
                        Layout.preferredWidth: 160
                    }

                    Rectangle {
                        Layout.fillWidth: true
                        height: 80
                        radius: 16
                        color: Theme.surfaceHover
                        border.width: nameInput.activeFocus ? 2 : 0
                        border.color: Theme.focusBorder

                        TextInput {
                            id: nameInput
                            anchors.fill: parent
                            anchors.leftMargin: 24
                            anchors.rightMargin: 24
                            anchors.topMargin: 20
                            anchors.bottomMargin: 20
                            text: root.newName
                            font.pixelSize: Theme.fontSmall
                            color: Theme.textPrimary
                            clip: true
                            verticalAlignment: TextInput.AlignVCenter
                            onTextChanged: root.newName = text
                            KeyNavigation.down: hostInput
                            Keys.onEscapePressed: {
                                root.resetForm();
                                serverList.forceActiveFocus();
                            }
                        }
                    }
                }

                // Host
                RowLayout {
                    spacing: 24

                    Text {
                        text: "Host"
                        font.pixelSize: Theme.fontSmall
                        color: Theme.textSecondary
                        Layout.preferredWidth: 160
                    }

                    Rectangle {
                        Layout.fillWidth: true
                        height: 80
                        radius: 16
                        color: Theme.surfaceHover
                        border.width: hostInput.activeFocus ? 2 : 0
                        border.color: Theme.focusBorder

                        TextInput {
                            id: hostInput
                            anchors.fill: parent
                            anchors.leftMargin: 24
                            anchors.rightMargin: 24
                            anchors.topMargin: 20
                            anchors.bottomMargin: 20
                            text: root.newHost
                            font.pixelSize: Theme.fontSmall
                            color: Theme.textPrimary
                            clip: true
                            verticalAlignment: TextInput.AlignVCenter
                            onTextChanged: root.newHost = text
                            KeyNavigation.up: nameInput
                            KeyNavigation.down: appInput
                            Keys.onEscapePressed: {
                                root.resetForm();
                                serverList.forceActiveFocus();
                            }
                        }
                    }
                }

                // App
                RowLayout {
                    spacing: 24

                    Text {
                        text: "App"
                        font.pixelSize: Theme.fontSmall
                        color: Theme.textSecondary
                        Layout.preferredWidth: 160
                    }

                    Rectangle {
                        Layout.fillWidth: true
                        height: 80
                        radius: 16
                        color: Theme.surfaceHover
                        border.width: appInput.activeFocus ? 2 : 0
                        border.color: Theme.focusBorder

                        TextInput {
                            id: appInput
                            anchors.fill: parent
                            anchors.leftMargin: 24
                            anchors.rightMargin: 24
                            anchors.topMargin: 20
                            anchors.bottomMargin: 20
                            text: root.newApp
                            font.pixelSize: Theme.fontSmall
                            color: Theme.textPrimary
                            clip: true
                            verticalAlignment: TextInput.AlignVCenter
                            onTextChanged: root.newApp = text
                            KeyNavigation.up: hostInput
                            Keys.onEscapePressed: {
                                root.resetForm();
                                serverList.forceActiveFocus();
                            }
                        }
                    }
                }

                // Action buttons
                RowLayout {
                    Layout.alignment: Qt.AlignRight
                    spacing: 16

                    FocusScope {
                        id: cancelScope
                        width: cancelBtn.width
                        height: cancelBtn.height

                        KeyNavigation.right: saveScope

                        SettingsButton {
                            id: cancelBtn
                            text: "Cancel"
                            focus: parent.activeFocus

                            MouseArea {
                                anchors.fill: parent
                                hoverEnabled: true
                                cursorShape: Qt.PointingHandCursor
                                onClicked: {
                                    root.resetForm();
                                    serverList.forceActiveFocus();
                                }
                            }
                        }

                        Keys.onReturnPressed: {
                            root.resetForm();
                            serverList.forceActiveFocus();
                        }
                    }

                    FocusScope {
                        id: saveScope
                        width: saveBtn.width
                        height: saveBtn.height

                        KeyNavigation.left: cancelScope

                        SettingsButton {
                            id: saveBtn
                            text: "Save"
                            focus: parent.activeFocus

                            MouseArea {
                                anchors.fill: parent
                                hoverEnabled: true
                                cursorShape: Qt.PointingHandCursor
                                onClicked: {
                                    root.addServer();
                                    serverList.forceActiveFocus();
                                }
                            }
                        }

                        Keys.onReturnPressed: {
                            root.addServer();
                            serverList.forceActiveFocus();
                        }
                    }
                }
            }
        }

        // Hint
        Text {
            text: root.showAddForm ? "Esc: Cancel" : "A: Select  |  Servers are launched from Home"
            font.pixelSize: Theme.fontHint
            color: Theme.textSecondary
            Layout.alignment: Qt.AlignHCenter
            visible: !root.showAddForm || root.showAddForm
        }
    }

    // Remove confirmation dialog
    Rectangle {
        anchors.fill: parent
        color: Qt.rgba(0, 0, 0, 0.7)
        visible: root.confirmRemoveIndex >= 0

        MouseArea {
            anchors.fill: parent
            onClicked: {
                root.confirmRemoveIndex = -1;
            }
        }

        Rectangle {
            anchors.centerIn: parent
            width: 800
            height: 350
            radius: 32
            color: Theme.surface

            ColumnLayout {
                anchors.centerIn: parent
                spacing: 32

                Text {
                    text: root.confirmRemoveIndex >= 0 && root.confirmRemoveIndex < root.servers.length ? "Remove \"" + root.servers[root.confirmRemoveIndex].name + "\"?" : ""
                    font.pixelSize: Theme.fontTitle
                    font.bold: true
                    color: Theme.textPrimary
                    Layout.alignment: Qt.AlignHCenter
                }

                RowLayout {
                    Layout.alignment: Qt.AlignHCenter
                    spacing: 32

                    FocusScope {
                        id: confirmRemoveYes
                        width: confirmRemoveYesBtn.width
                        height: confirmRemoveYesBtn.height

                        KeyNavigation.right: confirmRemoveNo

                        SettingsButton {
                            id: confirmRemoveYesBtn
                            text: "Remove"
                            focus: parent.activeFocus

                            MouseArea {
                                anchors.fill: parent
                                hoverEnabled: true
                                cursorShape: Qt.PointingHandCursor
                                onClicked: {
                                    root.removeServer(root.confirmRemoveIndex);
                                }
                            }
                        }

                        Keys.onReturnPressed: {
                            root.removeServer(root.confirmRemoveIndex);
                        }
                    }

                    FocusScope {
                        id: confirmRemoveNo
                        width: confirmRemoveNoBtn.width
                        height: confirmRemoveNoBtn.height
                        focus: root.confirmRemoveIndex >= 0

                        KeyNavigation.left: confirmRemoveYes

                        SettingsButton {
                            id: confirmRemoveNoBtn
                            text: "Cancel"
                            focus: parent.activeFocus

                            MouseArea {
                                anchors.fill: parent
                                hoverEnabled: true
                                cursorShape: Qt.PointingHandCursor
                                onClicked: {
                                    root.confirmRemoveIndex = -1;
                                }
                            }
                        }

                        Keys.onReturnPressed: {
                            root.confirmRemoveIndex = -1;
                        }
                        Keys.onEscapePressed: {
                            root.confirmRemoveIndex = -1;
                        }
                    }
                }
            }
        }

        Keys.onEscapePressed: {
            root.confirmRemoveIndex = -1;
        }
    }

    // Pairing dialog
    Rectangle {
        anchors.fill: parent
        color: Qt.rgba(0, 0, 0, 0.7)
        visible: root.pairingServerIndex >= 0
        z: 55

        MouseArea {
            anchors.fill: parent
            onClicked: {}
        }

        Rectangle {
            anchors.centerIn: parent
            width: 900
            height: 420
            radius: 32
            color: Theme.surface

            ColumnLayout {
                anchors.centerIn: parent
                spacing: 32

                Text {
                    text: "Pairing with " + (root.pairingServerIndex >= 0 && root.pairingServerIndex < root.servers.length ? root.servers[root.pairingServerIndex].name : "")
                    font.pixelSize: Theme.fontTitle
                    font.bold: true
                    color: Theme.textPrimary
                    Layout.alignment: Qt.AlignHCenter
                }

                Text {
                    text: root.pairingPin !== "" ? "Enter this PIN in the Sunshine web UI:" : "Waiting for PIN..."
                    font.pixelSize: Theme.fontBody
                    color: Theme.textSecondary
                    Layout.alignment: Qt.AlignHCenter
                }

                Text {
                    visible: root.pairingPin !== ""
                    text: root.pairingPin
                    font.pixelSize: Theme.fontHero
                    font.bold: true
                    color: Theme.ember
                    Layout.alignment: Qt.AlignHCenter
                }

                Text {
                    visible: root.pairingPin !== ""
                    text: root.pairingServerIndex >= 0 && root.pairingServerIndex < root.servers.length ? "https://" + root.servers[root.pairingServerIndex].host + ":47990" : ""
                    font.pixelSize: Theme.fontSmall
                    color: Theme.textMuted
                    Layout.alignment: Qt.AlignHCenter
                }

                FocusScope {
                    id: pairCancelScope
                    width: pairCancelBtn.width
                    height: pairCancelBtn.height
                    Layout.alignment: Qt.AlignHCenter
                    focus: root.pairingServerIndex >= 0

                    SettingsButton {
                        id: pairCancelBtn
                        text: "Cancel"
                        focus: parent.activeFocus

                        MouseArea {
                            anchors.fill: parent
                            hoverEnabled: true
                            cursorShape: Qt.PointingHandCursor
                            onClicked: root.cancelPairing()
                        }
                    }

                    Keys.onReturnPressed: root.cancelPairing()
                    Keys.onEscapePressed: root.cancelPairing()
                }
            }
        }

        Keys.onEscapePressed: root.cancelPairing()
    }
}
