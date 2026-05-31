import Quickshell.Io
import QtQuick

FocusScope {
    id: root

    property string shellState: ""
    property var targets: StreamProviders.active.targets
    property var runningWindows: []
    property string runningAppClass: ""
    property bool overlayDrawerOpen: false
    property bool avSystemOn: false
    property bool avWaking: false

    property var applications: AppDiscoveryManager.applications
    property alias homeScreen: homeScreen
    property alias settingsPanel: settingsPanel
    property alias navDrawer: navDrawer
    property alias overlay: overlay
    property alias notificationCenter: notificationCenter
    property alias powerOverlay: powerOverlay

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

    // Toggle the nav drawer — the focus-scoped `menu` action. Converges every
    // drawer-toggle surface: the gamepad Home-tap (shell.qml's onIntentHomeTap
    // when idle), the on-screen menu button, and the keyboard Tab path below.
    // If an overlay is open, the first press dismisses it instead of opening the
    // drawer. (The global return-to-shell escape lives in shell.qml's
    // onIntentHome, NOT here — `menu` never leaves a running app.)
    function toggleMenu() {
        if (powerOverlay.opened) {
            powerOverlay.opened = false;
            homeFocusTimer.restart();
        } else if (notificationCenter.opened) {
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

    // Keyboard drawer toggle (Tab), independent of Home. First-class keyboard
    // co-primary: the K400 on the couch opens the drawer without a controller.
    // Only meaningful on the home screen; when an app owns focus the shell
    // window isn't focused, so this never fires over a stream.
    Keys.onTabPressed: {
        if (root.shellState === "idle")
            toggleMenu();
    }

    // --- Keyboard debug capture (replaces the deleted daemon keys: snoop) ---
    // Rolling list of currently-held keyboard keys, read straight from Wayland
    // `Keys` on the layout root. The daemon stopped snooping the keyboard in
    // Phase 2, so the debug pane's keyboard half is now QML-native. Only used
    // when Theme.controllerDebug is on; the handlers never consume the event
    // (accepted stays false) so navigation is unaffected.
    property var _debugKeyNames: []

    function _debugKeyName(event) {
        // Friendly labels for the keys most worth seeing; fall back to the raw
        // text, else the numeric key code.
        switch (event.key) {
        case Qt.Key_Up:
            return "↑";
        case Qt.Key_Down:
            return "↓";
        case Qt.Key_Left:
            return "←";
        case Qt.Key_Right:
            return "→";
        case Qt.Key_Return:
        case Qt.Key_Enter:
            return "Enter";
        case Qt.Key_Escape:
            return "Esc";
        case Qt.Key_Tab:
            return "Tab";
        case Qt.Key_Backspace:
            return "Backspace";
        case Qt.Key_Space:
            return "Space";
        case Qt.Key_Meta:
        case Qt.Key_Super_L:
        case Qt.Key_Super_R:
            return "Meta";
        case Qt.Key_Control:
            return "Ctrl";
        case Qt.Key_Shift:
            return "Shift";
        case Qt.Key_Alt:
            return "Alt";
        }
        if (event.text && event.text.trim().length > 0)
            return event.text.toUpperCase();
        return "0x" + event.key.toString(16);
    }

    Keys.onPressed: event => {
        if (Theme.controllerDebug && !event.isAutoRepeat) {
            let name = _debugKeyName(event);
            let names = root._debugKeyNames.slice();
            if (names.indexOf(name) === -1) {
                names.push(name);
                root._debugKeyNames = names;
                debugOverlay.currentKeys = names.join(" + ");
            }
        }
        event.accepted = false;
    }

    Keys.onReleased: event => {
        if (Theme.controllerDebug && !event.isAutoRepeat) {
            let name = _debugKeyName(event);
            let names = root._debugKeyNames.filter(n => n !== name);
            root._debugKeyNames = names;
            debugOverlay.currentKeys = names.join(" + ");
        }
        event.accepted = false;
    }

    HomeScreen {
        id: homeScreen
        anchors.fill: parent
        visible: root.shellState === "idle"
        targets: root.targets
        shellState: root.shellState
        focus: root.shellState === "idle" && !settingsPanel.visible && !navDrawer.opened && !notificationCenter.opened && !powerOverlay.opened

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
        onPowerRequested: {
            powerOverlay.opened = true;
            powerOverlay.forceActiveFocus();
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
            if (notificationCenter.opened || errorLogViewer.opened || powerOverlay.opened)
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
        onPowerRequested: {
            navDrawer.opened = false;
            powerOverlay.opened = true;
            powerOverlay.forceActiveFocus();
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

    // === Power Overlay ===
    PowerOverlay {
        id: powerOverlay
        z: 60
        onCancelled: {
            powerOverlay.opened = false;
            homeFocusTimer.restart();
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
            onPowerRequested: {
                root.returnToShellRequested();
                powerOverlay.opened = true;
                powerOverlay.forceActiveFocus();
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

    Connections {
        target: powerOverlay
        function onOpenedChanged() {
            if (!powerOverlay.opened)
                homeFocusTimer.restart();
        }
    }

    // --- Debug Input Overlay ---
    // Controller `buttons:` come from the daemon subscribe stream; keyboard
    // keys are read QML-side from Wayland `Keys` (see the root key-capture
    // handlers below) since the daemon no longer snoops the keyboard.
    // See docs/IPC_PROTOCOL.md.
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

        SocketClient {
            id: debugSubscribe
            // Subscribe to the daemon's controller `buttons:` stream over a
            // native Quickshell socket (SocketClient, #97). The keyboard half of
            // the pane no longer comes from the daemon — the daemon stopped
            // snooping the keyboard (Phase 2). Keyboard keys are captured
            // QML-side from Wayland `Keys` on the layout root and fed into
            // `debugOverlay.currentKeys`. Auto-reconnects on drop.
            subscribe: true
            onLineReceived: line => {
                if (line === "subscribed")
                    return;
                if (line.startsWith("buttons:")) {
                    debugOverlay.currentCombo = line.substring(8).trim();
                }
            }
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
            id: inputFadeTimer
            interval: 1500
            onTriggered: {
                if (debugOverlay.currentInput === "")
                    debugOverlay.showingInput = false;
            }
        }

        onVisibleChanged: {
            if (!visible) {
                debugSubscribe.stop();
                showingInput = false;
                currentCombo = "";
                currentKeys = "";
                displayInput = "";
                root._debugKeyNames = [];
            } else {
                debugSubscribe.start();
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
