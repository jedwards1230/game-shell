import QtQuick
import QtQuick.Layouts
import Quickshell.Io

/**
 * NavigationDrawer — Left navigation drawer with Home, Settings, Force Quit,
 * controller capture toggle, and theme toggle.
 */
Drawer {
    id: root
    edge: "left"
    drawerWidth: 560

    signal forceQuitRequested()
    signal settingsRequested()

    // Controller grab state: "grabbed" or "released"
    property string _grabState: "grabbed"

    // === Force Quit Process ===
    Process {
        id: forceQuitProcess
        command: ["bash", "-c", "pkill -f moonlight; pkill -f steam; true"]
    }

    // === Input control processes ===
    Process {
        id: grabProcess
        command: ["python3", "-c", "import socket,os; s=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM); s.connect(os.environ.get('GAME_SHELL_SOCK','/run/user/'+str(os.getuid())+'/game-shell-input.sock')); s.sendall(b'grab\\n'); print(s.recv(64).decode().strip()); s.close()"]
        stdout: SplitParser {
            onRead: (line) => { if (line.trim() === "ok") root._grabState = "grabbed" }
        }
    }

    Process {
        id: releaseProcess
        command: ["python3", "-c", "import socket,os; s=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM); s.connect(os.environ.get('GAME_SHELL_SOCK','/run/user/'+str(os.getuid())+'/game-shell-input.sock')); s.sendall(b'release\\n'); print(s.recv(64).decode().strip()); s.close()"]
        stdout: SplitParser {
            onRead: (line) => { if (line.trim() === "ok") root._grabState = "released" }
        }
    }

    // Query grab state on open
    Process {
        id: statusProcess
        command: ["python3", "-c", "import socket,os; s=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM); s.connect(os.environ.get('GAME_SHELL_SOCK','/run/user/'+str(os.getuid())+'/game-shell-input.sock')); s.sendall(b'status\\n'); print(s.recv(64).decode().strip()); s.close()"]
        stdout: SplitParser {
            onRead: (line) => {
                let parts = line.trim().split(":")
                if (parts.length >= 2)
                    root._grabState = parts[1]
            }
        }
    }

    onOpenedChanged: {
        if (opened) {
            navList.currentIndex = 0
            navFocusTimer.restart()
            statusProcess.running = true
        }
    }

    Timer {
        id: navFocusTimer
        interval: 50
        onTriggered: navList.forceActiveFocus()
    }

    // === Navigation items model ===
    // type: "action" = clickable item, "toggle" = toggle switch, "separator" = visual divider
    property var _navItems: [
        { label: "Home",               icon: "\u{1F3E0}", type: "action",    action: "home" },
        { label: "Settings",           icon: "⚙",    type: "action",    action: "settings" },
        { label: "Force Quit",         icon: "⏹",    type: "action",    action: "forceQuit" },
        { label: "",                   icon: "",          type: "separator", action: "" },
        { label: "Controller Capture", icon: "\u{1F3AE}", type: "toggle",   action: "toggleGrab" },
        { label: "Dark / Light Mode",  icon: "\u{1F3A8}", type: "toggle",   action: "toggleTheme" },
        { label: "",                   icon: "",          type: "separator", action: "" },
        { label: "Settings",           icon: "⚙",    type: "action",    action: "settings" }
    ]

    // Indices that are focusable (skip separators)
    property var _focusableIndices: {
        let result = []
        for (let i = 0; i < _navItems.length; i++) {
            if (_navItems[i].type !== "separator")
                result.push(i)
        }
        return result
    }

    ColumnLayout {
        anchors.fill: parent
        spacing: 0

        // === Clock + Date Header ===
        Item {
            Layout.fillWidth: true
            Layout.preferredHeight: 160

            ColumnLayout {
                anchors.fill: parent
                anchors.leftMargin: 40
                anchors.topMargin: 24
                anchors.bottomMargin: 24
                spacing: 8

                Text {
                    id: drawerClock
                    font.pixelSize: Theme.fontTitle
                    font.bold: true
                    color: Theme.textPrimary

                    Timer {
                        interval: 1000
                        running: root.opened
                        repeat: true
                        triggeredOnStart: true
                        onTriggered: {
                            let now = new Date()
                            drawerClock.text = now.toLocaleTimeString(Qt.locale(), "h:mm AP")
                        }
                    }
                }

                Text {
                    id: drawerDate
                    font.pixelSize: Theme.fontSmall
                    color: Theme.textSecondary

                    Timer {
                        interval: 60000
                        running: root.opened
                        repeat: true
                        triggeredOnStart: true
                        onTriggered: {
                            let now = new Date()
                            drawerDate.text = now.toLocaleDateString(Qt.locale(), "dddd, MMMM d")
                        }
                    }
                }
            }
        }

        // Divider under clock
        Rectangle {
            Layout.fillWidth: true
            height: 2
            color: Theme.surfaceBorder
        }

        // === Navigation List ===
        ListView {
            id: navList
            Layout.fillWidth: true
            Layout.fillHeight: true
            model: root._navItems
            focus: true
            clip: true
            currentIndex: 0

            delegate: Item {
                required property int index
                required property var modelData

                width: navList.width
                height: modelData.type === "separator" ? 2 : 100

                // Separator
                Rectangle {
                    anchors.fill: parent
                    anchors.leftMargin: 24
                    anchors.rightMargin: 24
                    color: Theme.surfaceBorder
                    visible: modelData.type === "separator"
                }

                // Action / Toggle item
                Rectangle {
                    anchors.fill: parent
                    visible: modelData.type !== "separator"
                    color: {
                        if (navList.currentIndex === index && navList.activeFocus)
                            return Theme.surfaceHover
                        return "transparent"
                    }

                    Behavior on color { ColorAnimation { duration: 150 } }

                    // Left accent bar on focused item
                    Rectangle {
                        anchors.left: parent.left
                        anchors.verticalCenter: parent.verticalCenter
                        width: 4
                        height: parent.height - 16
                        radius: 2
                        color: Theme.focusBorder
                        visible: navList.currentIndex === index && navList.activeFocus
                    }

                    RowLayout {
                        anchors.fill: parent
                        anchors.leftMargin: 40
                        anchors.rightMargin: 40
                        spacing: 20

                        // Icon
                        Text {
                            text: modelData.icon
                            font.pixelSize: Theme.fontBody
                            Layout.preferredWidth: 64
                            horizontalAlignment: Text.AlignHCenter
                        }

                        // Label
                        Text {
                            text: modelData.label
                            font.pixelSize: Theme.fontBody
                            color: {
                                if (modelData.action === "forceQuit")
                                    return Theme.crimson
                                return Theme.textPrimary
                            }
                            font.bold: modelData.action === "forceQuit"
                            Layout.fillWidth: true
                        }

                        // Status text for toggles
                        Text {
                            visible: modelData.type === "toggle"
                            font.pixelSize: Theme.fontSmall
                            color: Theme.textMuted
                            text: {
                                if (modelData.action === "toggleGrab") {
                                    return root._grabState === "grabbed" ? "Capturing keys..." : "Keys released"
                                }
                                if (modelData.action === "toggleTheme") {
                                    if (Theme.themeMode === "dark") return "Dark"
                                    if (Theme.themeMode === "light") return "Light"
                                    return "Auto"
                                }
                                return ""
                            }
                        }
                    }

                    MouseArea {
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: root._activateItem(index)
                    }
                }
            }

            // Key navigation: skip separators
            Keys.onDownPressed: {
                let currentFocusIdx = root._focusableIndices.indexOf(navList.currentIndex)
                if (currentFocusIdx < root._focusableIndices.length - 1) {
                    navList.currentIndex = root._focusableIndices[currentFocusIdx + 1]
                }
            }

            Keys.onUpPressed: {
                let currentFocusIdx = root._focusableIndices.indexOf(navList.currentIndex)
                if (currentFocusIdx > 0) {
                    navList.currentIndex = root._focusableIndices[currentFocusIdx - 1]
                }
            }

            Keys.onReturnPressed: root._activateItem(navList.currentIndex)
        }

        // === Hint Bar ===
        Rectangle {
            Layout.fillWidth: true
            height: 80
            color: Theme.surfaceHover

            Text {
                anchors.centerIn: parent
                text: "A: Select  |  B: Close"
                font.pixelSize: Theme.fontHint
                color: Theme.textSecondary
            }
        }
    }

    function _activateItem(index) {
        let item = _navItems[index]
        if (!item || item.type === "separator") return

        switch (item.action) {
            case "home":
                root.opened = false
                root.closed()
                break
            case "settings":
                root.opened = false
                root.closed()
                root.settingsRequested()
                break
            case "forceQuit":
                forceQuitProcess.running = true
                root.forceQuitRequested()
                break
            case "toggleGrab":
                if (root._grabState === "grabbed")
                    releaseProcess.running = true
                else
                    grabProcess.running = true
                break
            case "toggleTheme":
                if (Theme.themeMode === "dark") Theme.setThemeMode("light")
                else if (Theme.themeMode === "light") Theme.setThemeMode("auto")
                else Theme.setThemeMode("dark")
                break
        }
    }
}
