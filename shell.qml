import Quickshell
import Quickshell.Io
import Quickshell.Wayland
import QtQuick
import QtQuick.Layouts
import "components" as Components

ShellRoot {
    id: root

    property string state: "idle"
    property var currentTarget: null
    property int crashCount: 0
    property var targets: []
    property string runningAppClass: ""
    property bool avSystemOn: false
    property bool avWaking: false
    property var _pendingApp: null
    property var runningWindows: []
    property bool overlayDrawerOpen: false

    Process {
        id: loadTargets
        command: ["cat", "/opt/game-shell/targets.json"]
        stdout: SplitParser {
            onRead: (line) => {
                try { root.targets = JSON.parse(line) }
                catch(e) { console.log("Failed to parse targets:", e) }
            }
        }
    }

    Process {
        id: avStatusCheck
        command: ["/usr/local/bin/living-room-cec", "status"]
        stdout: SplitParser {
            onRead: (line) => {
                var match = line.match(/^\s*(AVR)\s*:\s*(\S+)/i)
                if (match) {
                    root.avSystemOn = (match[2].toLowerCase() === "on")
                }
            }
        }
    }

    Timer {
        id: avStatusPoll
        interval: 30000
        running: root.state === "idle"
        repeat: true
        triggeredOnStart: true
        onTriggered: { if (!avStatusCheck.running) avStatusCheck.running = true }
    }

    Timer {
        id: avWakeCooldown
        interval: 30000
    }


    Component.onCompleted: { loadTargets.running = true; comboListener.running = true }

    Timer {
        id: crashResetTimer
        interval: 300000
        running: root.state === "streaming"
        onTriggered: { root.crashCount = 0 }
    }

    Process {
        id: avWake
        command: ["/usr/local/bin/living-room-cec", "on"]
        onExited: (exitCode) => {
            root.avWaking = false
            if (exitCode === 0 && !avStatusCheck.running)
                avStatusCheck.running = true
        }
    }

    Process {
        id: moonlight
        onExited: (exitCode, exitStatus) => {
            if (exitCode === 0) {
                overlay.hide()
                root.state = "idle"
                grabInput()
            } else {
                root.crashCount++
                if (root.crashCount < 5) {
                    root.state = "reconnecting"
                    overlay.show("Reconnecting... (" + root.crashCount + "/5)")
                    reconnectTimer.start()
                } else {
                    overlay.show("Stream failed after 5 attempts")
                    errorDismissTimer.start()
                    root.state = "idle"
                    grabInput()
                }
            }
        }
    }

    Timer {
        id: reconnectTimer
        interval: 2000
        onTriggered: launchMoonlight()
    }

    Timer {
        id: errorDismissTimer
        interval: 5000
        onTriggered: { overlay.hide() }
    }

    Process {
        id: inputGrab
        command: ["python3", "-c", "import socket,os; s=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM); s.connect(os.environ.get('GAME_SHELL_SOCK','/run/user/'+str(os.getuid())+'/game-shell-input.sock')); s.sendall(b'grab\\n'); print(s.recv(64).decode().strip()); s.close()"]
    }

    Process {
        id: inputRelease
        command: ["python3", "-c", "import socket,os; s=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM); s.connect(os.environ.get('GAME_SHELL_SOCK','/run/user/'+str(os.getuid())+'/game-shell-input.sock')); s.sendall(b'release\\n'); print(s.recv(64).decode().strip()); s.close()"]
    }

    Process {
        id: endSession
        command: ["/usr/local/bin/end-game-session"]
    }

    // Always-on combo listener — handles force-quit (Back+Home+LB+RB) from any state
    Process {
        id: comboListener
        command: ["python3", "-c", "import socket,os;s=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM);s.connect(os.environ.get('GAME_SHELL_SOCK','/run/user/'+str(os.getuid())+'/game-shell-input.sock'));s.sendall(b'subscribe\\n');[print(l,flush=True) for d in iter(lambda:s.recv(1024),b'') for l in d.decode().splitlines()]"]
        stdout: SplitParser {
            onRead: (line) => {
                if (line === "combo:force-quit") root.forceQuit()
                else if (line === "combo:end-session") endSession.running = true
                else if (line === "input-mode:mouse") {
                    Components.Theme.mouseMode = true
                }
                else if (line === "input-mode:controller") {
                    Components.Theme.mouseMode = false
                }
                else if (line === "controller-wake" && root.state === "idle" && !avWake.running && !avWakeCooldown.running) {
                    avWake.running = true
                    avWakeCooldown.restart()
                }
                else if (line === "home-press") {
                    if (root.state === "appRunning") {
                        root.overlayDrawerOpen = !root.overlayDrawerOpen
                    }
                }
                else if (line === "combo:home-hold") {
                    if (root.state === "appRunning") {
                        root.closeAndReturnToShell()
                    }
                }
            }
        }
        onExited: { comboReconnect.start() }
    }

    Timer {
        id: comboReconnect
        interval: 2000
        onTriggered: { comboListener.running = true }
    }

    function forceQuit() {
        moonlight.running = false
        forceKill.running = true
        if (root.state === "appRunning") closeAndReturnToShell()
        root.overlayDrawerOpen = false
        root.state = "idle"
        grabInput()
        navDrawer.opened = false
        settingsPanel.visible = false
        homeFocusTimer.restart()
    }

    Process {
        id: forceKill
        command: ["bash", "-c", "pkill -f moonlight; pkill -f steam; true"]
    }

    Process {
        id: closeAppWindow
        property string appClass: ""
        command: ["hyprctl", "dispatch", "closewindow", "class:" + appClass]
    }

    property var _prelaunchClasses: []

    function launchDesktopApp(app) {
        root.state = "appRunning"
        root.runningAppClass = ""
        snapshotClients.running = true
        appRunner.command = ["hyprctl", "dispatch", "exec", app.exec || app.name]
        appRunner.running = true
        detectNewWindow.restart()
    }

    function returnToShell() {
        root.runningAppClass = ""
        root.overlayDrawerOpen = false
        root.state = "idle"
        grabInput()
        settingsPanel.visible = false
        homeFocusTimer.restart()
    }

    function closeAndReturnToShell() {
        if (root.runningAppClass !== "") {
            closeAppWindow.appClass = root.runningAppClass
            closeAppWindow.running = true
        }
        returnToShell()
    }

    Process {
        id: appRunner
        command: ["echo"]
    }

    Components.HyprctlClients {
        id: snapshotClients
        onClientsReceived: (clients) => {
            root._prelaunchClasses = clients.map(c => c["class"])
        }
        onErrorOccurred: { root._prelaunchClasses = [] }
    }

    Components.HyprctlClients {
        id: detectClient
        onClientsReceived: (clients) => {
            for (let i = 0; i < clients.length; i++) {
                if (root._prelaunchClasses.indexOf(clients[i]["class"]) < 0 && clients[i]["class"] !== "") {
                    root.runningAppClass = clients[i]["class"]
                    break
                }
            }
        }
    }

    Timer {
        id: detectNewWindow
        interval: 2000
        onTriggered: { detectClient.running = true }
    }

    Components.HyprctlClients {
        id: windowQuery
        onClientsReceived: (clients) => {
            root._handleWindowQueryResult(clients)
        }
        onErrorOccurred: { root._handleWindowQueryResult([]) }
    }

    Process {
        id: focusWindow
        property string windowClass: ""
        command: ["hyprctl", "dispatch", "focuswindow", "class:" + windowClass]
        onExited: (exitCode) => {
            if (exitCode !== 0 && root.state === "appRunning")
                root.returnToShell()
        }
    }

    function checkAndLaunchApp(app) {
        root._pendingApp = app
        windowQuery.running = true
    }

    function _handleWindowQueryResult(clients) {
        let app = root._pendingApp
        if (!app) return
        root._pendingApp = null

        let matchClass = (app.wmClass || "").toLowerCase()
        let matchExec = (app.exec || "").split(/\s/)[0].split("/").pop().toLowerCase()
        let matchName = (app.name || "").toLowerCase()

        for (let i = 0; i < clients.length; i++) {
            let cls = (clients[i]["class"] || "").toLowerCase()
            let initCls = (clients[i]["initialClass"] || "").toLowerCase()
            if ((matchClass && (cls === matchClass || initCls === matchClass)) ||
                (matchExec && (cls === matchExec || initCls === matchExec)) ||
                (matchName && (cls === matchName || initCls === matchName))) {
                root.runningAppClass = clients[i]["class"]
                focusWindow.windowClass = clients[i]["class"]
                focusWindow.running = true
                root.state = "appRunning"
                return
            }
        }

        root.launchDesktopApp(app)
    }

    Components.HyprctlClients {
        id: windowPoller
        onClientsReceived: (clients) => {
            let apps = (typeof homeScreen !== "undefined" && homeScreen) ? homeScreen.applications : []
            let windows = []
            for (let i = 0; i < clients.length; i++) {
                let c = clients[i]
                if (c["class"] && c["class"] !== "" && c["class"].indexOf("quickshell") < 0) {
                    let cls = c["class"]
                    let iconName = (c["initialClass"] || cls).toLowerCase()
                    let appIcon = iconName
                    for (let j = 0; j < apps.length; j++) {
                        let a = apps[j]
                        let wm = (a.wmClass || "").toLowerCase()
                        let ex = (a.exec || "").split(/\s/)[0].split("/").pop().toLowerCase()
                        if (wm === cls.toLowerCase() || ex === cls.toLowerCase() || (a.name || "").toLowerCase() === cls.toLowerCase()) {
                            appIcon = a.icon || iconName
                            break
                        }
                    }
                    windows.push({
                        windowClass: cls,
                        title: c["title"] || cls,
                        name: c["title"] || cls,
                        icon: appIcon,
                        exec: ""
                    })
                }
            }
            root.runningWindows = windows

            if (root.state === "appRunning" && root.runningAppClass !== "") {
                let found = false
                for (let i = 0; i < root.runningWindows.length; i++) {
                    if (root.runningWindows[i].windowClass === root.runningAppClass) {
                        found = true
                        break
                    }
                }
                if (!found) root.returnToShell()
            }
        }
        onErrorOccurred: { root.runningWindows = [] }
    }

    Timer {
        id: windowPollTimer
        interval: root.state === "appRunning" ? 2000 : 5000
        running: root.state === "idle" || root.state === "appRunning"
        repeat: true
        triggeredOnStart: true
        onTriggered: { if (!windowPoller.running) windowPoller.running = true }
    }

    function launchStream(target) {
        root.currentTarget = target
        root.state = "launching"
        root.crashCount = 0
        overlay.show("Launching " + (target.app || target.name) + "...")
        avWake.running = true
        launchMoonlight()
    }

    function launchMoonlight() {
        let args = ["moonlight", "stream", currentTarget.host, currentTarget.app]
        if (currentTarget.resolution === "3840x2160") args.push("--4k")
        if (currentTarget.fps) { args.push("--fps"); args.push(String(currentTarget.fps)) }
        if (currentTarget.hdr) args.push("--hdr")
        if (currentTarget.codec) { args.push("--video-codec"); args.push(currentTarget.codec) }
        args.push("--display-mode", "fullscreen")
        args.push("--no-quit-after")
        args.push("--no-frame-pacing")

        moonlight.command = args
        releaseInput()
        root.state = "streaming"
        moonlight.running = true
    }

    function grabInput() { inputGrab.running = true }
    function releaseInput() { inputRelease.running = true }

    Variants {
        model: Quickshell.screens

        PanelWindow {
            required property var modelData
            screen: modelData
            visible: root.state !== "appRunning" || root.overlayDrawerOpen

            anchors {
                top: true
                bottom: true
                left: true
                right: true
            }

            color: root.state === "appRunning" ? "transparent" : Components.Theme.background
            focusable: true

            Item {
                anchors.fill: parent

                // Guide button (KEY_HOMEPAGE) toggles navigation drawer
                // Always claim CEC active source (wake if off, switch input if on different source)
                Keys.onPressed: (event) => {
                    if (event.key === Qt.Key_HomePage) {
                        if (root.state === "idle" && !avWake.running && !avWakeCooldown.running) {
                            if (!root.avSystemOn) root.avWaking = true
                            avWake.running = true
                            avWakeCooldown.restart()
                        }
                        navDrawer.opened = !navDrawer.opened
                        if (navDrawer.opened)
                            navDrawer.forceActiveFocus()
                        else
                            homeFocusTimer.restart()
                        event.accepted = true
                    }
                }

                Components.HomeScreen {
                    id: homeScreen
                    anchors.fill: parent
                    visible: root.state === "idle"
                    targets: root.targets
                    shellState: root.state
                    focus: root.state === "idle" && !settingsPanel.visible && !navDrawer.opened

                    runningWindows: root.runningWindows

                    onStreamRequested: (target) => root.launchStream(target)
                    onAppLaunchRequested: (app) => root.checkAndLaunchApp(app)
                    onAppFocusRequested: (windowClass) => {
                        root.runningAppClass = windowClass
                        focusWindow.windowClass = windowClass
                        focusWindow.running = true
                        root.state = "appRunning"
                    }
                    onSettingsRequested: {
                        settingsPanel.visible = true
                        settingsPanel.forceActiveFocus()
                    }
                }

                Components.SettingsPanel {
                    id: settingsPanel
                    anchors.fill: parent
                    onClosed: {
                        settingsPanel.visible = false
                        homeFocusTimer.restart()
                    }
                }

                Timer {
                    id: homeFocusTimer
                    interval: 50
                    onTriggered: { homeScreen.forceActiveFocus() }
                }

                Components.StreamOverlay {
                    id: overlay
                    anchors.fill: parent
                }

                // === Navigation Drawer (idle state) ===
                Components.NavigationDrawer {
                    id: navDrawer
                    z: 50
                    visible: root.state === "idle"
                    onSettingsRequested: {
                        navDrawer.opened = false
                        settingsPanel.visible = true
                        settingsPanel.forceActiveFocus()
                    }
                    onHomeSelected: {
                        navDrawer.opened = false
                        settingsPanel.visible = false
                        homeFocusTimer.restart()
                    }
                    onClosed: {
                        navDrawer.opened = false
                        homeFocusTimer.restart()
                    }
                }

                // === Overlay Drawer (appRunning state) ===
                Rectangle {
                    anchors.fill: parent
                    color: Qt.rgba(0, 0, 0, 0.5)
                    visible: root.state === "appRunning" && root.overlayDrawerOpen
                    z: 50

                    Components.NavigationDrawer {
                        id: overlayNavDrawer
                        overlayMode: true
                        opened: root.overlayDrawerOpen
                        onHomeSelected: {
                            root.returnToShell()
                        }
                        onSettingsRequested: {
                            root.overlayDrawerOpen = false
                            root.returnToShell()
                            settingsPanel.visible = true
                            settingsPanel.forceActiveFocus()
                        }
                        onClosed: {
                            root.overlayDrawerOpen = false
                        }
                    }

                    Keys.onEscapePressed: {
                        root.overlayDrawerOpen = false
                    }
                }

                // --- AV Wake Overlay ---
                Rectangle {
                    anchors.fill: parent
                    color: Qt.rgba(0, 0, 0, 0.7)
                    visible: root.avWaking
                    z: 40

                    Text {
                        anchors.centerIn: parent
                        text: "Waking AV System..."
                        font.pixelSize: Components.Theme.fontTitle
                        color: Components.Theme.textOnDark
                    }
                }

                Timer {
                    id: avWakeTimeout
                    interval: 20000
                    running: root.avWaking
                    onTriggered: { root.avWaking = false }
                }



                // --- Debug Input Overlay ---
                // Shows currently-held buttons as a combo display.
                // Only visible when buttons are pressed and controllerDebug is on.
                Item {
                    id: debugOverlay
                    anchors.fill: parent
                    visible: Components.Theme.controllerDebug
                    z: 100

                    property string currentCombo: ""
                    property string displayCombo: ""
                    property bool showingCombo: false

                    Process {
                        id: debugSubscribe
                        command: ["python3", "-c", "import socket,os;s=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM);s.connect(os.environ.get('GAME_SHELL_SOCK','/run/user/'+str(os.getuid())+'/game-shell-input.sock'));s.sendall(b'subscribe\\n');[print(l,flush=True) for d in iter(lambda:s.recv(1024),b'') for l in d.decode().splitlines()]"]
                        stdout: SplitParser {
                            onRead: (line) => {
                                if (line === "subscribed") return
                                if (line.startsWith("buttons:")) {
                                    let combo = line.substring(8).trim()
                                    debugOverlay.currentCombo = combo
                                    if (combo !== "") {
                                        debugOverlay.displayCombo = combo
                                        debugOverlay.showingCombo = true
                                        comboFadeTimer.restart()
                                    }
                                }
                            }
                        }
                        onExited: {
                            if (debugOverlay.visible)
                                reconnectDebug.start()
                        }
                    }

                    Timer {
                        id: reconnectDebug
                        interval: 2000
                        onTriggered: {
                            if (debugOverlay.visible)
                                debugSubscribe.running = true
                        }
                    }

                    Timer {
                        id: comboFadeTimer
                        interval: 1500
                        onTriggered: {
                            if (debugOverlay.currentCombo === "")
                                debugOverlay.showingCombo = false
                        }
                    }

                    onVisibleChanged: {
                        if (!visible) {
                            debugSubscribe.running = false
                            reconnectDebug.running = false
                            showingCombo = false
                            currentCombo = ""
                            displayCombo = ""
                        } else {
                            debugSubscribe.running = true
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
                        border.color: Components.Theme.ember
                        visible: debugOverlay.showingCombo
                        opacity: debugOverlay.currentCombo !== "" ? 1.0 : 0.4

                        Behavior on opacity { NumberAnimation { duration: 300 } }

                        Text {
                            id: comboText
                            anchors.centerIn: parent
                            text: debugOverlay.displayCombo
                            font.pixelSize: Components.Theme.fontBody
                            font.bold: true
                            color: Components.Theme.textOnDark
                        }
                    }
                }

            }
        }

    }
}
