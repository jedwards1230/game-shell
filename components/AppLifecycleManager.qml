import Quickshell.Io
import QtQuick

Item {
    id: root

    property string runningAppClass: ""
    property var runningWindows: []
    property var applications: []
    property string shellState: ""

    property var _prelaunchClasses: []
    property var _pendingApp: null
    property var _launchedApps: Object.create(null)
    property int _maxMisses: 3

    signal appLaunched()
    signal appClosed()

    function launchDesktopApp(app) {
        runningAppClass = ""
        snapshotClients.running = true
        appRunner.command = ["hyprctl", "dispatch", "exec", app.exec || app.name]
        appRunner.running = true
        detectNewWindow.restart()

        // Track launched app for resilient window matching
        let key = (app.wmClass || app.name || "").toLowerCase()
        if (key !== "") {
            let tracked = _launchedApps
            tracked[key] = { app: app, misses: 0, windowClass: "" }
            _launchedApps = tracked
        }

        appLaunched()
    }

    function checkAndLaunchApp(app) {
        _pendingApp = app
        windowQuery.running = true
    }

    function closeApp() {
        if (runningAppClass !== "") {
            closeAppWindow.appClass = runningAppClass
            closeAppWindow.running = true
        }
    }

    function focusApp(windowClass) {
        runningAppClass = windowClass
        focusWindow.windowClass = windowClass
        focusWindow.running = true
        appLaunched()
    }

    onShellStateChanged: {
        if (shellState === "idle") {
            if (!windowPoller.running) windowPoller.running = true
        }
    }

    Process {
        id: closeAppWindow
        property string appClass: ""
        command: ["hyprctl", "dispatch", "closewindow", "class:" + appClass]
    }

    Process {
        id: appRunner
        command: ["echo"]
    }

    HyprctlClients {
        id: snapshotClients
        onClientsReceived: (clients) => {
            root._prelaunchClasses = clients.map(c => c["class"])
        }
        onErrorOccurred: { root._prelaunchClasses = [] }
    }

    HyprctlClients {
        id: detectClient
        onClientsReceived: (clients) => {
            for (let i = 0; i < clients.length; i++) {
                if (root._prelaunchClasses.indexOf(clients[i]["class"]) < 0 && clients[i]["class"] !== "") {
                    root.runningAppClass = clients[i]["class"]

                    // Store discovered window class in _launchedApps
                    let tracked = root._launchedApps
                    for (let key in tracked) {
                        if (tracked[key].windowClass === "" && WindowMatcher.matchesApp(tracked[key].app, clients[i])) {
                            tracked[key].windowClass = clients[i]["class"]
                            break
                        }
                    }
                    root._launchedApps = tracked

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

    HyprctlClients {
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
            if (exitCode !== 0 && root.shellState === "appRunning")
                root.appClosed()
        }
    }

    function _handleWindowQueryResult(clients) {
        let app = _pendingApp
        if (!app) return
        _pendingApp = null

        for (let i = 0; i < clients.length; i++) {
            if (WindowMatcher.matchesApp(app, clients[i])) {
                root.runningAppClass = clients[i]["class"]
                focusWindow.windowClass = clients[i]["class"]
                focusWindow.running = true
                appLaunched()
                return
            }
        }

        launchDesktopApp(app)
    }

    HyprctlClients {
        id: windowPoller
        onClientsReceived: (clients) => {
            let apps = (root.applications || [])
            let windows = []
            let seenClasses = Object.create(null)
            for (let i = 0; i < clients.length; i++) {
                let c = clients[i]
                let cls = c["class"] || ""
                if (cls === "" || cls.indexOf("quickshell") >= 0) continue
                if (seenClasses[cls]) continue
                seenClasses[cls] = true

                let iconName = (c["initialClass"] || cls).toLowerCase()
                let appIcon = iconName
                let appName = c["title"] || cls

                // Use WindowMatcher for icon/name resolution
                for (let j = 0; j < apps.length; j++) {
                    if (WindowMatcher.matchesApp(apps[j], c)) {
                        appIcon = apps[j].icon || iconName
                        appName = apps[j].name || appName
                        break
                    }
                }

                windows.push({
                    windowClass: cls,
                    title: c["title"] || cls,
                    name: appName,
                    icon: appIcon,
                    exec: ""
                })
            }
            root.runningWindows = windows

            // Track miss counts in _launchedApps
            let tracked = root._launchedApps
            let trackedChanged = false
            for (let key in tracked) {
                let entry = tracked[key]
                let wc = entry.windowClass
                let found = false

                if (wc !== "" && seenClasses[wc]) {
                    found = true
                } else {
                    // Try matching by app metadata
                    for (let i = 0; i < clients.length; i++) {
                        if (WindowMatcher.matchesApp(entry.app, clients[i])) {
                            found = true
                            if (wc === "") {
                                entry.windowClass = clients[i]["class"]
                                trackedChanged = true
                            }
                            break
                        }
                    }
                }

                if (found) {
                    if (entry.misses > 0) {
                        entry.misses = 0
                        trackedChanged = true
                    }
                } else {
                    entry.misses++
                    trackedChanged = true
                    if (entry.misses >= root._maxMisses) {
                        delete tracked[key]
                    }
                }
            }
            if (trackedChanged) root._launchedApps = tracked

            // Only fire appClosed when in appRunning state and foreground app is truly gone
            if (root.shellState === "appRunning" && root.runningAppClass !== "") {
                let found = false
                for (let i = 0; i < windows.length; i++) {
                    if (windows[i].windowClass === root.runningAppClass) {
                        found = true
                        break
                    }
                }
                if (!found) root.appClosed()
            }
        }
        onErrorOccurred: (message) => {
            console.warn("AppLifecycleManager: window poll error:", message)
        }
    }

    Timer {
        id: windowPollTimer
        interval: root.shellState === "appRunning" ? 2000 : 5000
        running: root.shellState === "idle" || root.shellState === "appRunning"
        repeat: true
        triggeredOnStart: true
        onTriggered: { if (!windowPoller.running) windowPoller.running = true }
    }
}
