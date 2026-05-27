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

    signal streamRequested(var target)
    signal appLaunchRequested(var app)
    signal appFocusRequested(string windowClass)
    signal appCloseRequested(string windowClass)
    signal homeKeyPressed
    signal returnToShellRequested
    signal overlayDrawerClosed

    // Session conflict dialog — driven by StreamManager signals
    property alias sessionDialog: sessionDialog

    function focusHome() {
        homeFocusTimer.restart();
    }

    Keys.onPressed: event => {
        if (event.key === Qt.Key_HomePage) {
            root.homeKeyPressed();
            navDrawer.opened = !navDrawer.opened;
            if (navDrawer.opened)
                navDrawer.forceActiveFocus();
            else
                homeFocusTimer.restart();
            event.accepted = true;
        }
    }

    Shortcut {
        sequence: "Tab"
        enabled: NotificationManager.hasActiveError && root.shellState === "idle" && !homeScreen.activeFocus
        onActivated: {
            NotificationManager.dismissErrors();
            errorLogViewer.opened = true;
            errorLogViewer.forceActiveFocus();
        }
    }

    HomeScreen {
        id: homeScreen
        anchors.fill: parent
        visible: root.shellState === "idle"
        targets: root.targets
        shellState: root.shellState
        focus: root.shellState === "idle" && !settingsPanel.visible && !navDrawer.opened

        runningWindows: root.runningWindows

        onStreamRequested: target => root.streamRequested(target)
        onAppLaunchRequested: app => root.appLaunchRequested(app)
        onAppFocusRequested: windowClass => root.appFocusRequested(windowClass)
        onAppCloseRequested: windowClass => root.appCloseRequested(windowClass)
        onSettingsRequested: {
            settingsPanel.visible = true;
            settingsPanel.forceActiveFocus();
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

    // --- Debug Input Overlay ---
    // IPC: subscribes to buttons:* events for real-time display (see docs/IPC_PROTOCOL.md)
    Item {
        id: debugOverlay
        anchors.fill: parent
        visible: Theme.controllerDebug
        z: 100

        property string currentCombo: ""
        property string displayCombo: ""
        property bool showingCombo: false

        Process {
            id: debugSubscribe
            command: ["python3", "-c", "import socket,os;s=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM);s.connect(os.environ.get('GAME_SHELL_SOCK','/run/user/'+str(os.getuid())+'/game-shell-input.sock'));s.sendall(b'subscribe\\n');[print(l,flush=True) for d in iter(lambda:s.recv(1024),b'') for l in d.decode().splitlines()]"]
            stdout: SplitParser {
                onRead: line => {
                    if (line === "subscribed")
                        return;
                    if (line.startsWith("buttons:")) {
                        let combo = line.substring(8).trim();
                        debugOverlay.currentCombo = combo;
                        if (combo !== "") {
                            debugOverlay.displayCombo = combo;
                            debugOverlay.showingCombo = true;
                            comboFadeTimer.restart();
                        }
                    }
                }
            }
            onExited: {
                if (debugOverlay.visible)
                    reconnectDebug.start();
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
            id: comboFadeTimer
            interval: 1500
            onTriggered: {
                if (debugOverlay.currentCombo === "")
                    debugOverlay.showingCombo = false;
            }
        }

        onVisibleChanged: {
            if (!visible) {
                debugSubscribe.running = false;
                reconnectDebug.running = false;
                showingCombo = false;
                currentCombo = "";
                displayCombo = "";
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
            visible: debugOverlay.showingCombo
            opacity: debugOverlay.currentCombo !== "" ? 1.0 : 0.4

            Behavior on opacity {
                NumberAnimation {
                    duration: 300
                }
            }

            Text {
                id: comboText
                anchors.centerIn: parent
                text: debugOverlay.displayCombo
                font.pixelSize: Theme.fontBody
                font.bold: true
                color: Theme.textOnDark
            }
        }
    }
}
