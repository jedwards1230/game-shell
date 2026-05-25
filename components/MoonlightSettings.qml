import QtQuick
import QtQuick.Layouts
import Quickshell.Io

Item {
    id: root

    property var servers: []
    property bool showAddForm: false
    property int confirmRemoveIndex: -1

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
            onRead: (line) => {
                try { root.servers = JSON.parse(line) }
                catch(e) { root.servers = [] }
            }
        }
    }

    Process {
        id: saveServers
        property string json: "[]"
        command: ["bash", "-c", "echo '" + json + "' > /opt/game-shell/targets.json"]
    }

    function persistServers() {
        saveServers.json = JSON.stringify(root.servers)
        saveServers.running = true
    }

    function addServer() {
        if (newName === "" || newHost === "") return
        let entry = {
            name: newName,
            host: newHost,
            app: newApp,
            resolution: newResolution,
            fps: newFps,
            hdr: newHdr,
            codec: newCodec
        }
        let list = root.servers.slice()
        list.push(entry)
        root.servers = list
        persistServers()
        resetForm()
    }

    function removeServer(idx) {
        let list = root.servers.slice()
        list.splice(idx, 1)
        root.servers = list
        persistServers()
        root.confirmRemoveIndex = -1
    }

    function resetForm() {
        newName = ""
        newHost = ""
        newApp = "Desktop"
        newResolution = "3840x2160"
        newFps = 120
        newHdr = true
        newCodec = "HEVC"
        showAddForm = false
    }

    Component.onCompleted: { loadServers.running = true }

    onVisibleChanged: {
        if (visible) {
            loadServers.running = true
            if (serverList.count > 0) serverList.forceActiveFocus()
            else addBtnScope.forceActiveFocus()
        }
    }

    ColumnLayout {
        anchors.fill: parent
        anchors.margins: Theme.padding
        spacing: 24

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
            focus: !root.showAddForm

            delegate: Rectangle {
                required property int index
                required property var modelData
                width: serverList.width
                height: 160
                radius: Theme.cardRadius
                color: serverList.currentIndex === index && serverList.activeFocus
                       ? Theme.surfaceHover : Theme.surface
                border.width: 2
                border.color: Theme.surfaceBorder

                Behavior on color { ColorAnimation { duration: 150 } }

                RowLayout {
                    anchors.fill: parent
                    anchors.margins: 24
                    spacing: 24

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
                                onClicked: { root.confirmRemoveIndex = index }
                            }
                        }

                        Keys.onReturnPressed: { root.confirmRemoveIndex = index }
                    }
                }
            }

            Keys.onReturnPressed: {
                // no-op: servers are launched from home screen
            }

            Keys.onDownPressed: {
                if (currentIndex < root.servers.length - 1) currentIndex++
                else addBtnScope.forceActiveFocus()
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
                        addBtnScope.forceActiveFocus()
                        root.showAddForm = true
                        nameInput.forceActiveFocus()
                    }
                }
            }

            Keys.onReturnPressed: {
                root.showAddForm = true
                nameInput.forceActiveFocus()
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
                anchors.margins: 32
                spacing: 20

                Text {
                    text: "New Server"
                    font.pixelSize: Theme.fontBody
                    font.bold: true
                    color: Theme.textPrimary
                }

                // Name
                RowLayout {
                    spacing: 16

                    Text {
                        text: "Name"
                        font.pixelSize: Theme.fontSmall
                        color: Theme.textSecondary
                        Layout.preferredWidth: 200
                    }

                    Rectangle {
                        Layout.fillWidth: true
                        height: 64
                        radius: 12
                        color: Theme.surfaceHover
                        border.width: nameInput.activeFocus ? 2 : 0
                        border.color: Theme.focusBorder

                        TextInput {
                            id: nameInput
                            anchors.fill: parent
                            anchors.margins: 16
                            text: root.newName
                            font.pixelSize: Theme.fontSmall
                            color: Theme.textPrimary
                            clip: true
                            onTextChanged: root.newName = text
                            KeyNavigation.down: hostInput
                            Keys.onEscapePressed: { root.resetForm(); serverList.forceActiveFocus() }
                        }
                    }
                }

                // Host
                RowLayout {
                    spacing: 16

                    Text {
                        text: "Host"
                        font.pixelSize: Theme.fontSmall
                        color: Theme.textSecondary
                        Layout.preferredWidth: 200
                    }

                    Rectangle {
                        Layout.fillWidth: true
                        height: 64
                        radius: 12
                        color: Theme.surfaceHover
                        border.width: hostInput.activeFocus ? 2 : 0
                        border.color: Theme.focusBorder

                        TextInput {
                            id: hostInput
                            anchors.fill: parent
                            anchors.margins: 16
                            text: root.newHost
                            font.pixelSize: Theme.fontSmall
                            color: Theme.textPrimary
                            clip: true
                            onTextChanged: root.newHost = text
                            KeyNavigation.up: nameInput
                            KeyNavigation.down: appInput
                            Keys.onEscapePressed: { root.resetForm(); serverList.forceActiveFocus() }
                        }
                    }
                }

                // App
                RowLayout {
                    spacing: 16

                    Text {
                        text: "App"
                        font.pixelSize: Theme.fontSmall
                        color: Theme.textSecondary
                        Layout.preferredWidth: 200
                    }

                    Rectangle {
                        Layout.fillWidth: true
                        height: 64
                        radius: 12
                        color: Theme.surfaceHover
                        border.width: appInput.activeFocus ? 2 : 0
                        border.color: Theme.focusBorder

                        TextInput {
                            id: appInput
                            anchors.fill: parent
                            anchors.margins: 16
                            text: root.newApp
                            font.pixelSize: Theme.fontSmall
                            color: Theme.textPrimary
                            clip: true
                            onTextChanged: root.newApp = text
                            KeyNavigation.up: hostInput
                            Keys.onEscapePressed: { root.resetForm(); serverList.forceActiveFocus() }
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
                                onClicked: { root.resetForm(); serverList.forceActiveFocus() }
                            }
                        }

                        Keys.onReturnPressed: { root.resetForm(); serverList.forceActiveFocus() }
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
                                onClicked: { root.addServer(); serverList.forceActiveFocus() }
                            }
                        }

                        Keys.onReturnPressed: { root.addServer(); serverList.forceActiveFocus() }
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
            onClicked: { root.confirmRemoveIndex = -1 }
        }

        Rectangle {
            anchors.centerIn: parent
            width: 700
            height: 300
            radius: 32
            color: Theme.surface

            ColumnLayout {
                anchors.centerIn: parent
                spacing: 32

                Text {
                    text: root.confirmRemoveIndex >= 0 && root.confirmRemoveIndex < root.servers.length
                          ? "Remove \"" + root.servers[root.confirmRemoveIndex].name + "\"?"
                          : ""
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
                        focus: root.confirmRemoveIndex >= 0

                        KeyNavigation.right: confirmRemoveNo

                        SettingsButton {
                            id: confirmRemoveYesBtn
                            text: "Remove"
                            focus: parent.activeFocus

                            MouseArea {
                                anchors.fill: parent
                                hoverEnabled: true
                                cursorShape: Qt.PointingHandCursor
                                onClicked: { root.removeServer(root.confirmRemoveIndex) }
                            }
                        }

                        Keys.onReturnPressed: { root.removeServer(root.confirmRemoveIndex) }
                    }

                    FocusScope {
                        id: confirmRemoveNo
                        width: confirmRemoveNoBtn.width
                        height: confirmRemoveNoBtn.height

                        KeyNavigation.left: confirmRemoveYes

                        SettingsButton {
                            id: confirmRemoveNoBtn
                            text: "Cancel"
                            focus: parent.activeFocus

                            MouseArea {
                                anchors.fill: parent
                                hoverEnabled: true
                                cursorShape: Qt.PointingHandCursor
                                onClicked: { root.confirmRemoveIndex = -1 }
                            }
                        }

                        Keys.onReturnPressed: { root.confirmRemoveIndex = -1 }
                        Keys.onEscapePressed: { root.confirmRemoveIndex = -1 }
                    }
                }
            }
        }

        Keys.onEscapePressed: { root.confirmRemoveIndex = -1 }
    }
}
