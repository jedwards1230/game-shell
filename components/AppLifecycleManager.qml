import Quickshell.Io
import QtQuick

Item {
    id: root

    property string runningAppClass: ""
    property var runningWindows: []
    property var applications: []
    property string shellState: ""

    // Last active-window class reported by the daemon's `hypr:activewindow`
    // subscribe event (empty when no window is focused). Mirrors the compositor's
    // focus state without an extra query.
    property string activeWindowClass: ""
    // Last fullscreen state reported by the daemon's `hypr:fullscreen` event.
    property bool activeWindowFullscreen: false

    property var _prelaunchClasses: []
    property var _pendingApp: null
    property var _launchedApps: Object.create(null)
    property int _maxMisses: 3

    signal appLaunched
    signal appClosed

    function launchDesktopApp(app) {
        runningAppClass = "";
        snapshotClients.running = true;
        appRunner._appName = app.name || "";
        appRunner.command = ["hyprctl", "dispatch", "exec", app.exec || app.name];
        appRunner.running = true;
        detectNewWindow.restart();

        // Track launched app for resilient window matching
        let key = (app.wmClass || app.name || "").toLowerCase();
        if (key !== "") {
            let tracked = _launchedApps;
            tracked[key] = {
                app: app,
                misses: 0,
                windowClass: ""
            };
            _launchedApps = tracked;
        }

        appLaunched();
    }

    function checkAndLaunchApp(app) {
        _pendingApp = app;
        windowQuery.running = true;
    }

    function closeApp() {
        if (runningAppClass !== "") {
            closeAppWindow.appClass = runningAppClass;
            closeAppWindow.running = true;
        }
    }

    function closeAppByClass(windowClass) {
        if (windowClass && windowClass !== "") {
            closeAppWindow.appClass = windowClass;
            closeAppWindow.running = true;
        }
    }

    function focusApp(windowClass) {
        runningAppClass = windowClass;
        focusWindow.windowClass = windowClass;
        focusWindow.running = true;
        appLaunched();
    }

    onShellStateChanged: {
        if (shellState === "idle") {
            if (!windowPoller.running)
                windowPoller.running = true;
        }
    }

    Process {
        id: closeAppWindow
        property string appClass: ""
        command: ["hyprctl", "dispatch", "closewindow", "class:" + appClass]
    }

    Process {
        id: appRunner
        property string _appName: ""
        command: ["echo"]
        onExited: (exitCode, exitStatus) => {
            if (exitCode !== 0) {
                let cmd = appRunner.command.join(" ");
                ErrorLog.log("app", "Failed to launch " + (_appName || "application"), "Command: " + cmd + "\nExit code: " + exitCode, _appName);
            }
        }
    }

    HyprctlClients {
        id: snapshotClients
        onClientsReceived: clients => {
            root._prelaunchClasses = clients.map(c => c["class"]);
        }
        onErrorOccurred: {
            root._prelaunchClasses = [];
        }
    }

    HyprctlClients {
        id: detectClient
        onClientsReceived: clients => {
            for (let i = 0; i < clients.length; i++) {
                if (root._prelaunchClasses.indexOf(clients[i]["class"]) < 0 && clients[i]["class"] !== "") {
                    root.runningAppClass = clients[i]["class"];

                    // Store discovered window class in _launchedApps
                    let tracked = root._launchedApps;
                    for (let key in tracked) {
                        if (tracked[key].windowClass === "" && WindowMatcher.matchesApp(tracked[key].app, clients[i])) {
                            tracked[key].windowClass = clients[i]["class"];
                            break;
                        }
                    }
                    root._launchedApps = tracked;

                    break;
                }
            }
        }
    }

    Timer {
        id: detectNewWindow
        interval: 2000
        onTriggered: {
            detectClient.running = true;
        }
    }

    HyprctlClients {
        id: windowQuery
        onClientsReceived: clients => {
            root._handleWindowQueryResult(clients);
        }
        onErrorOccurred: {
            root._handleWindowQueryResult([]);
        }
    }

    Process {
        id: focusWindow
        property string windowClass: ""
        command: ["hyprctl", "dispatch", "focuswindow", "class:" + windowClass]
        onExited: exitCode => {
            if (exitCode !== 0 && root.shellState === "appRunning")
                root.appClosed();
            else
                ensureFullscreen.running = true;
        }
    }

    // Window rule only applies at creation — restore fullscreen on resume
    Process {
        id: ensureFullscreen
        command: ["bash", "-c", "FS=$(hyprctl activewindow -j | grep -o '\"fullscreen\": [0-9]*' | grep -o '[0-9]*'); [ \"$FS\" = \"0\" ] && hyprctl dispatch fullscreen 0; exit 0"]
    }

    function _handleWindowQueryResult(clients) {
        let app = _pendingApp;
        if (!app)
            return;
        _pendingApp = null;

        for (let i = 0; i < clients.length; i++) {
            if (WindowMatcher.matchesApp(app, clients[i])) {
                root.runningAppClass = clients[i]["class"];
                focusWindow.windowClass = clients[i]["class"];
                focusWindow.running = true;
                appLaunched();
                return;
            }
        }

        launchDesktopApp(app);
    }

    HyprctlClients {
        id: windowPoller
        onClientsReceived: clients => {
            let apps = (root.applications || []);
            let windows = [];
            let seenClasses = Object.create(null);
            for (let i = 0; i < clients.length; i++) {
                let c = clients[i];
                let cls = c["class"] || "";
                if (cls === "" || cls.indexOf("quickshell") >= 0)
                    continue;
                if (seenClasses[cls])
                    continue;
                seenClasses[cls] = true;

                let iconName = (c["initialClass"] || cls).toLowerCase();
                let appIcon = iconName;
                let appName = c["title"] || cls;

                // Use WindowMatcher for icon/name resolution
                for (let j = 0; j < apps.length; j++) {
                    if (WindowMatcher.matchesApp(apps[j], c)) {
                        appIcon = apps[j].icon || iconName;
                        appName = apps[j].name || appName;
                        break;
                    }
                }

                windows.push({
                    windowClass: cls,
                    title: c["title"] || cls,
                    name: appName,
                    icon: appIcon,
                    exec: ""
                });
            }
            root.runningWindows = windows;

            // Track miss counts in _launchedApps
            let tracked = root._launchedApps;
            let trackedChanged = false;
            for (let key in tracked) {
                let entry = tracked[key];
                let wc = entry.windowClass;
                let found = false;

                if (wc !== "" && seenClasses[wc]) {
                    found = true;
                } else {
                    // Try matching by app metadata
                    for (let i = 0; i < clients.length; i++) {
                        if (WindowMatcher.matchesApp(entry.app, clients[i])) {
                            found = true;
                            if (wc === "") {
                                entry.windowClass = clients[i]["class"];
                                trackedChanged = true;
                            }
                            break;
                        }
                    }
                }

                if (found) {
                    if (entry.misses > 0) {
                        entry.misses = 0;
                        trackedChanged = true;
                    }
                } else {
                    entry.misses++;
                    trackedChanged = true;
                    if (entry.misses >= root._maxMisses) {
                        delete tracked[key];
                    }
                }
            }
            if (trackedChanged)
                root._launchedApps = tracked;

            // Only fire appClosed when in appRunning state and foreground app is truly gone
            if (root.shellState === "appRunning" && root.runningAppClass !== "") {
                let found = false;
                for (let i = 0; i < windows.length; i++) {
                    if (windows[i].windowClass === root.runningAppClass) {
                        found = true;
                        break;
                    }
                }
                if (!found)
                    root.appClosed();
            }
        }
        onErrorOccurred: message => {
            console.warn("AppLifecycleManager: window poll error:", message);
        }
    }

    Timer {
        id: windowPollTimer
        interval: root.shellState === "appRunning" ? 2000 : 5000
        running: root.shellState === "idle" || root.shellState === "appRunning"
        repeat: true
        triggeredOnStart: true
        onTriggered: {
            if (!windowPoller.running)
                windowPoller.running = true;
        }
    }

    // Subscribe to the daemon's Hyprland window events (hypr:activewindow,
    // hypr:fullscreen — see docs/IPC_PROTOCOL.md) so window open/close/focus
    // changes are reflected immediately instead of waiting for the next poll
    // tick. The periodic windowPoller above remains the source of truth for the
    // runningWindows model and appClosed detection; these events just kick an
    // extra poll on transitions, so the public behavior is unchanged.
    SocketClient {
        id: hyprEventListener
        // Subscribe stream over a native Quickshell socket (SocketClient, #97);
        // filter to `hypr:` lines (the stream also carries high-frequency
        // buttons:/intent:* events we don't want here). Auto-reconnects on drop.
        subscribe: true
        onLineReceived: line => {
            if (line.indexOf("hypr:activewindow:") === 0) {
                root.activeWindowClass = line.substring("hypr:activewindow:".length);
                root._onHyprWindowEvent();
            } else if (line.indexOf("hypr:fullscreen:") === 0) {
                root.activeWindowFullscreen = line.substring("hypr:fullscreen:".length) === "1";
                root._onHyprWindowEvent();
            }
        }
    }

    function _onHyprWindowEvent() {
        // Kick an immediate poll on window transitions while the shell is the
        // active state owner; the poller itself guards against re-entry.
        if ((root.shellState === "idle" || root.shellState === "appRunning") && !windowPoller.running)
            windowPoller.running = true;
    }

    Component.onCompleted: {
        hyprEventListener.start();
    }
}
