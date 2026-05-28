import Quickshell.Io
import QtQuick

FocusScope {
    id: root

    property string shellState: ""
    property var targets: []
    property var runningWindows: []
    property string runningAppClass: ""
    property bool overlayDrawerOpen: false
    property bool avSystemOn: false
    property bool avWaking: false

    property var applications: homeScreen.applications
    property alias homeScreen: homeScreen
    property alias settingsPanel: settingsPanel
    property alias navDrawer: navDrawer
    property alias overlay: overlay
    property alias notificationCenter: notificationCenter

    signal streamRequested(var target)
    signal streamQuitRequested(var target)
    signal appLaunchRequested(var app)
    signal appFocusRequested(string windowClass)
    signal appCloseRequested(string windowClass)
    signal returnToShellRequested
    signal overlayDrawerClosed

    // Session conflict dialog — driven by StreamManager signals
    property alias sessionDialog: sessionDialog

    function focusHome() {
        homeFocusTimer.restart();
    }

    // Drawer-toggle behavior for the "home tap" action, called from
    // shell.qml's onHomePressed when in idle state. Single source of
    // truth — controller Home (socket), keyboard Meta (Hyprland-bound
    // and routed through the daemon), and any future "home tap" surface
    // all converge here via the home-press socket event.
    function handleHomeTap() {
        if (notificationCenter.opened) {
            notificationCenter.opened = false;
            homeFocusTimer.restart();
        } else {
            navDrawer.opened = !navDrawer.opened;
            if (navDrawer.opened)
                navDrawer.forceActiveFocus();
            else
                homeFocusTimer.restart();
        }
    }

    HomeScreen {
        id: homeScreen
        anchors.fill: parent
        visible: root.shellState === "idle"
        targets: root.targets
        shellState: root.shellState
        focus: root.shellState === "idle" && !settingsPanel.visible && !navDrawer.opened && !notificationCenter.opened

        runningWindows: root.runningWindows

        onStreamRequested: target => root.streamRequested(target)
        onStreamQuitRequested: target => root.streamQuitRequested(target)
        onAppLaunchRequested: app => root.appLaunchRequested(app)
        onAppFocusRequested: windowClass => root.appFocusRequested(windowClass)
        onAppCloseRequested: windowClass => root.appCloseRequested(windowClass)
        onSettingsRequested: {
            settingsPanel.visible = true;
            settingsPanel.forceActiveFocus();
        }
        onNotificationCenterRequested: {
            notificationCenter.opened = true;
            notificationCenter.forceActiveFocus();
        }
    }

    SettingsPanel {
        id: settingsPanel
        anchors.fill: parent
        onClosed: {
            settingsPanel.visible = false;
            homeFocusTimer.restart();
        }
    }

    Timer {
        id: homeFocusTimer
        interval: 50
        onTriggered: {
            if (notificationCenter.opened || errorLogViewer.opened)
                return;
            homeScreen.forceActiveFocus();
        }
    }

    StreamOverlay {
        id: overlay
        anchors.fill: parent
    }

    SessionDialog {
        id: sessionDialog
    }

    // === Navigation Drawer (idle state) ===
    NavigationDrawer {
        id: navDrawer
        z: 50
        visible: root.shellState === "idle"
        onSettingsRequested: {
            navDrawer.opened = false;
            settingsPanel.visible = true;
            settingsPanel.forceActiveFocus();
        }
        onNotificationCenterRequested: {
            navDrawer.opened = false;
            notificationCenter.opened = true;
            notificationCenter.forceActiveFocus();
        }
        onHomeSelected: {
            navDrawer.opened = false;
            settingsPanel.visible = false;
            homeFocusTimer.restart();
        }
        onClosed: {
            navDrawer.opened = false;
            homeFocusTimer.restart();
        }
    }

    // === Notification Stack ===
    NotificationStack {
        anchors.right: parent.right
        anchors.top: parent.top
        anchors.rightMargin: Units.spacingXL
        anchors.topMargin: Units.gridUnit * 9
        z: 45
    }

    // === Error Log Viewer ===
    ErrorLogViewer {
        id: errorLogViewer
        z: 60
    }

    // === Notification Center ===
    NotificationCenter {
        id: notificationCenter
        z: 60
        onErrorLogRequested: {
            errorLogViewer.opened = true;
            errorLogViewer.forceActiveFocus();
            notificationCenter.opened = false;
        }
    }

    // === Overlay Drawer (appRunning state) ===
    Item {
        anchors.fill: parent
        visible: root.shellState === "appRunning" && root.overlayDrawerOpen
        z: 50

        DimmedBackdrop {
            dimLevel: 0.5
            onClicked: root.overlayDrawerClosed()
        }

        NavigationDrawer {
            id: overlayNavDrawer
            overlayMode: true
            opened: root.overlayDrawerOpen
            onHomeSelected: {
                root.returnToShellRequested();
            }
            onSettingsRequested: {
                root.overlayDrawerClosed();
                root.returnToShellRequested();
                settingsPanel.visible = true;
                settingsPanel.forceActiveFocus();
            }
            onNotificationCenterRequested: {
                root.returnToShellRequested();
                notificationCenter.opened = true;
                notificationCenter.forceActiveFocus();
            }
            onClosed: {
                root.overlayDrawerClosed();
            }
        }

        Keys.onEscapePressed: {
            root.overlayDrawerClosed();
        }
    }

    // --- AV Wake Overlay ---
    DimmedBackdrop {
        visible: root.avWaking
        z: 40
        dimLevel: 0.7
        message: "Waking AV System..."
    }

    Connections {
        target: notificationCenter
        function onOpenedChanged() {
            if (!notificationCenter.opened && !errorLogViewer.opened)
                homeFocusTimer.restart();
        }
    }

    Connections {
        target: errorLogViewer
        function onOpenedChanged() {
            if (!errorLogViewer.opened)
                homeFocusTimer.restart();
        }
    }

    // --- Debug Input Overlay ---
    // Subscribes to the daemon socket for buttons:* (controller) and
    // keys:* (keyboard) events. See docs/IPC_PROTOCOL.md.
    Item {
        id: debugOverlay
        anchors.fill: parent
        visible: Theme.controllerDebug
        z: 100

        property string currentCombo: ""
        property string currentKeys: ""
        property string displayInput: ""
        property bool showingInput: false

        readonly property string currentInput: {
            if (currentCombo !== "" && currentKeys !== "")
                return currentCombo + " + " + currentKeys;
            return currentCombo !== "" ? currentCombo : currentKeys;
        }

        Process {
            id: debugSubscribe
            // Enables daemon-side `kbd-key` logging for this session and
            // then subscribes to events. Re-sent on every reconnect so a
            // daemon restart still results in logging being on while the
            // overlay is visible.
            command: ["python3", "-c", "import socket,os;s=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM);s.connect(os.environ.get('GAME_SHELL_SOCK','/run/user/'+str(os.getuid())+'/game-shell-input.sock'));s.sendall(b'kbd-log on\\nsubscribe\\n');[print(l,flush=True) for d in iter(lambda:s.recv(1024),b'') for l in d.decode().splitlines()]"]
            stdout: SplitParser {
                onRead: line => {
                    if (line === "subscribed" || line === "ok")
                        return;
                    if (line.startsWith("buttons:")) {
                        debugOverlay.currentCombo = line.substring(8).trim();
                    } else if (line.startsWith("keys:")) {
                        debugOverlay.currentKeys = line.substring(5).trim();
                    }
                }
            }
            onExited: {
                if (debugOverlay.visible)
                    reconnectDebug.start();
            }
        }

        // One-shot to turn off daemon-side `kbd-key` logging when the
        // debug overlay closes (or controllerDebug is disabled).
        Process {
            id: kbdLogOff
            command: ["python3", "-c", "import socket,os;s=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM);s.connect(os.environ.get('GAME_SHELL_SOCK','/run/user/'+str(os.getuid())+'/game-shell-input.sock'));s.sendall(b'kbd-log off\\n');s.recv(64);s.close()"]
        }

        // Pin displayInput to the latest non-empty value and hold it
        // briefly so a quick tap is still readable.
        onCurrentInputChanged: {
            if (currentInput !== "") {
                displayInput = currentInput;
                showingInput = true;
                inputFadeTimer.restart();
            }
        }

        Timer {
            id: reconnectDebug
            interval: 2000
            onTriggered: {
                if (debugOverlay.visible)
                    debugSubscribe.running = true;
            }
        }

        Timer {
            id: inputFadeTimer
            interval: 1500
            onTriggered: {
                if (debugOverlay.currentInput === "")
                    debugOverlay.showingInput = false;
            }
        }

        onVisibleChanged: {
            if (!visible) {
                debugSubscribe.running = false;
                reconnectDebug.running = false;
                showingInput = false;
                currentCombo = "";
                currentKeys = "";
                displayInput = "";
                kbdLogOff.running = true;
            } else {
                debugSubscribe.running = true;
            }
        }

        Rectangle {
            anchors.right: parent.right
            anchors.bottom: parent.bottom
            anchors.rightMargin: 48
            anchors.bottomMargin: 48
            width: comboText.implicitWidth + 64
            height: comboText.implicitHeight + 32
            radius: 16
            color: Qt.rgba(0, 0, 0, 0.8)
            border.width: 2
            border.color: Theme.ember
            visible: debugOverlay.showingInput
            opacity: debugOverlay.currentInput !== "" ? 1.0 : 0.4

            Behavior on opacity {
                NumberAnimation {
                    duration: 300
                }
            }

            Text {
                id: comboText
                anchors.centerIn: parent
                text: debugOverlay.displayInput
                font.pixelSize: Theme.fontBody
                font.bold: true
                color: Theme.textOnDark
            }
        }
    }
}
