import QtQuick
import QtQuick.Layouts
import Quickshell.Io

FocusScope {
    id: root

    property var targets: []
    property var applications: []
    property var recentApps: []
    property string shellState: "idle"

    // App-view: discovered apps per host { "host": ["App1", "App2", ...] }
    property var hostApps: ({})
    property int _appDiscoveryIndex: -1
    property bool _appDiscoveryRunning: false

    signal streamRequested(var target)
    signal appLaunchRequested(var app)
    signal settingsRequested()

    // Load installed applications
    Process {
        id: loadApps
        command: ["python3", "-c", `
import os, json, configparser
apps = []
seen = set()
for d in ['/usr/share/applications', os.path.expanduser('~/.local/share/applications')]:
    if not os.path.isdir(d): continue
    for f in sorted(os.listdir(d)):
        if not f.endswith('.desktop'): continue
        cp = configparser.ConfigParser(interpolation=None)
        cp.read(os.path.join(d, f))
        if not cp.has_section('Desktop Entry'): continue
        if cp.get('Desktop Entry', 'NoDisplay', fallback='false').lower() == 'true': continue
        if cp.get('Desktop Entry', 'Hidden', fallback='false').lower() == 'true': continue
        if cp.get('Desktop Entry', 'Type', fallback='') != 'Application': continue
        name = cp.get('Desktop Entry', 'Name', fallback='')
        if not name or name in seen: continue
        seen.add(name)
        ex = cp.get('Desktop Entry', 'Exec', fallback='')
        for tok in ['%u','%U','%f','%F','%i','%c','%k']:
            ex = ex.replace(tok, '')
        apps.append({'name': name, 'exec': ex.strip(), 'icon': cp.get('Desktop Entry', 'Icon', fallback=''), 'comment': cp.get('Desktop Entry', 'Comment', fallback='')})
apps.sort(key=lambda x: x['name'].lower())
print(json.dumps(apps))
`]
        stdout: SplitParser {
            onRead: (line) => {
                try { root.applications = JSON.parse(line) }
                catch(e) { console.log("Failed to parse apps:", e) }
            }
        }
    }

    // Load recent launches
    Process {
        id: loadRecents
        command: ["python3", "-c", `
import json, os
path = os.path.expanduser('~/.local/share/game-shell/recents.json')
try:
    with open(path) as f:
        data = json.load(f)
    print(json.dumps(data[:15]))
except:
    print('[]')
`]
        stdout: SplitParser {
            onRead: (line) => {
                try { root.recentApps = JSON.parse(line) }
                catch(e) { root.recentApps = [] }
            }
        }
    }

    Component.onCompleted: {
        loadApps.running = true
        loadRecents.running = true
    }

    function launchApp(app) {
        root.appLaunchRequested(app)
        recentsTracker.command = ["python3", "-c",
            "import json,os,time; p=os.path.expanduser('~/.local/share/game-shell/recents.json'); os.makedirs(os.path.dirname(p),exist_ok=True); " +
            "d=[]; " +
            "try:\n with open(p) as f: d=json.load(f)\nexcept: pass\n" +
            "entry={'name':'" + (app.name||"").replace("'","\\'") + "','exec':'" + (app.exec||"").replace("'","\\'") + "','comment':'" + (app.comment||"").replace("'","\\'") + "','time':time.time()}; " +
            "d=[e for e in d if e.get('name')!=entry['name']]; d.insert(0,entry); d=d[:20]; " +
            "open(p,'w').write(json.dumps(d,indent=2))"
        ]
        recentsTracker.running = true
        recentsReloadTimer.start()
    }

    Process { id: recentsTracker; command: ["echo"] }
    Timer { id: recentsReloadTimer; interval: 500; onTriggered: loadRecents.running = true }

    // === Moonlight App Discovery ===
    Process {
        id: appDiscovery
        property string currentHost: ""
        command: ["moonlight", "list", currentHost]
        stdout: SplitParser {
            onRead: (line) => {
                // moonlight list outputs lines like "1. Desktop" or just "Desktop"
                let trimmed = line.trim()
                if (trimmed === "" || trimmed.indexOf("Search") === 0 || trimmed.indexOf("Connect") === 0) return
                // Strip leading number+dot if present (e.g., "1. Desktop" -> "Desktop")
                let match = trimmed.match(/^\d+\.\s+(.+)/)
                let appName = match ? match[1] : trimmed
                if (appName === "") return

                let updated = root.hostApps
                if (!updated[appDiscovery.currentHost])
                    updated[appDiscovery.currentHost] = []
                updated[appDiscovery.currentHost].push(appName)
                root.hostApps = updated
            }
        }
        onExited: (exitCode, exitStatus) => {
            if (exitCode !== 0) {
                // Host offline or moonlight list failed — mark empty
                let updated = root.hostApps
                updated[appDiscovery.currentHost] = []
                root.hostApps = updated
            }
            // Discover next host
            root._appDiscoveryIndex++
            root._discoverNextHost()
        }
    }

    function _discoverNextHost() {
        if (_appDiscoveryIndex >= root.targets.length) {
            _appDiscoveryRunning = false
            // Force re-evaluation by reassigning
            root.hostApps = JSON.parse(JSON.stringify(root.hostApps))
            return
        }
        let target = root.targets[_appDiscoveryIndex]
        appDiscovery.currentHost = target.host || ""
        if (appDiscovery.currentHost === "") {
            _appDiscoveryIndex++
            _discoverNextHost()
            return
        }
        // Clear previous results for this host before re-query
        let updated = root.hostApps
        updated[appDiscovery.currentHost] = []
        root.hostApps = updated
        appDiscovery.running = true
    }

    function discoverAllApps() {
        if (_appDiscoveryRunning) return
        _appDiscoveryRunning = true
        _appDiscoveryIndex = 0
        // Clear all
        root.hostApps = {}
        _discoverNextHost()
    }

    // Refresh app discovery every 60 seconds when in app-view mode
    Timer {
        id: appDiscoveryTimer
        interval: 60000
        running: Theme.moonlightViewMode === "apps"
        repeat: true
        onTriggered: root.discoverAllApps()
    }

    // Trigger discovery when targets arrive or view mode switches to apps
    onTargetsChanged: {
        if (Theme.moonlightViewMode === "apps" && root.targets.length > 0)
            discoverAllApps()
    }

    Connections {
        target: Theme
        function onMoonlightViewModeChanged() {
            if (Theme.moonlightViewMode === "apps" && root.targets.length > 0)
                root.discoverAllApps()
        }
    }

    // Helper: find the ListView inside an app-view delegate by objectName
    function _focusAppViewRow(idx) {
        if (idx < 0 || idx >= appViewRepeater.count) return false
        let item = appViewRepeater.itemAt(idx)
        if (!item) return false
        // Find the ListView by objectName
        let listView = _findChild(item, "appViewListView")
        if (listView) { listView.forceActiveFocus(); return true }
        return false
    }

    function _findChild(parent, name) {
        for (let i = 0; i < parent.children.length; i++) {
            let child = parent.children[i]
            if (child.objectName === name) return child
            let found = _findChild(child, name)
            if (found) return found
        }
        return null
    }

    // Computed model for app-view rows, re-evaluated when targets or hostApps change
    property var _appViewRows: {
        // Explicitly reference both properties so QML re-evaluates this binding
        let ha = root.hostApps
        let tgts = root.targets
        let rows = []
        for (let i = 0; i < tgts.length; i++) {
            let t = tgts[i]
            let apps = ha[t.host] || []
            rows.push({ host: t.host, name: t.name, apps: apps, target: t })
        }
        return rows
    }

    ColumnLayout {
        anchors.fill: parent
        anchors.margins: Theme.padding
        spacing: 24

        // === Hero Clock Area ===
        RowLayout {
            Layout.fillWidth: true
            Layout.preferredHeight: 480
            spacing: 32

            // Clock + date (left side)
            ColumnLayout {
                spacing: 24
                Layout.alignment: Qt.AlignVCenter

                Text {
                    id: heroClockText
                    font.pixelSize: Theme.fontHero
                    font.bold: true
                    color: Theme.textPrimary

                    Timer {
                        interval: 1000
                        running: true
                        repeat: true
                        triggeredOnStart: true
                        onTriggered: {
                            let now = new Date()
                            heroClockText.text = now.toLocaleTimeString(Qt.locale(), "h:mm AP")
                        }
                    }
                }

                Text {
                    id: heroDateText
                    font.pixelSize: Theme.fontTitle
                    color: Theme.textSecondary

                    Timer {
                        interval: 60000
                        running: true
                        repeat: true
                        triggeredOnStart: true
                        onTriggered: {
                            let now = new Date()
                            heroDateText.text = now.toLocaleDateString(Qt.locale(), "dddd, MMMM d")
                        }
                    }
                }
            }

            Item { Layout.fillWidth: true }

            // Status icons (right side)
            StatusIcons {
                id: statusIcons
                Layout.alignment: Qt.AlignTop | Qt.AlignRight
                onSettingsRequested: root.settingsRequested()
                onFocusDownRequested: {
                    if (recentsRow.visible)
                        recentsRow.forceActiveFocus()
                    else if (Theme.moonlightViewMode === "servers")
                        moonlightRow.forceActiveFocus()
                    else if (!root._focusAppViewRow(0))
                        appsRow.forceActiveFocus()
                }
            }
        }

        // === Recents Row ===
        Text {
            visible: root.recentApps.length > 0
            text: "Recent"
            font.pixelSize: Theme.fontTitle
            font.bold: true
            color: Theme.textPrimary
        }

        Item {
            id: recentsContainer
            visible: root.recentApps.length > 0
            Layout.fillWidth: true
            Layout.preferredHeight: visible ? Theme.rowHeight : 0

            ListView {
                id: recentsRow
                anchors.fill: parent
                anchors.topMargin: -16
                anchors.bottomMargin: -16
                orientation: ListView.Horizontal
                spacing: Theme.cardSpacing
                focus: visible
                clip: false
                highlightMoveDuration: 150
                highlightMoveVelocity: -1
                keyNavigationEnabled: true

                model: root.recentApps

                delegate: AppCard {
                    required property int index
                    required property var modelData
                    height: Theme.cardHeight
                    width: Theme.cardWidth
                    app: modelData
                    focus: index === recentsRow.currentIndex
                    onActivated: root.launchApp(modelData)
                }

                Keys.onReturnPressed: {
                    if (recentsRow.currentItem)
                        root.launchApp(recentsRow.currentItem.modelData)
                }
                Keys.onEnterPressed: {
                    if (recentsRow.currentItem)
                        root.launchApp(recentsRow.currentItem.modelData)
                }
                Keys.onUpPressed: statusIcons.forceActiveFocus()
                Keys.onDownPressed: {
                    if (Theme.moonlightViewMode === "servers") {
                        moonlightRow.forceActiveFocus()
                    } else {
                        // App view: focus the first host row
                        if (!root._focusAppViewRow(0))
                            appsRow.forceActiveFocus()
                    }
                }
                Keys.onEscapePressed: root.settingsRequested()
            }
        }

        // === Moonlight Section (server-view or app-view) ===

        // Server view: single "Moonlight" row with one card per server
        Text {
            visible: Theme.moonlightViewMode === "servers"
            text: "Moonlight"
            font.pixelSize: Theme.fontTitle
            font.bold: true
            color: Theme.textPrimary
        }

        Item {
            visible: Theme.moonlightViewMode === "servers"
            Layout.fillWidth: true
            Layout.preferredHeight: visible ? Theme.rowHeight : 0

            ListView {
                id: moonlightRow
                anchors.fill: parent
                anchors.topMargin: -16
                anchors.bottomMargin: -16
                orientation: ListView.Horizontal
                spacing: Theme.cardSpacing
                focus: Theme.moonlightViewMode === "servers" && !recentsRow.visible
                clip: false
                highlightMoveDuration: 150
                highlightMoveVelocity: -1
                keyNavigationEnabled: true

                model: root.targets

                delegate: StreamCard {
                    required property int index
                    required property var modelData
                    height: Theme.cardHeight
                    width: Theme.cardWidth
                    target: modelData
                    focus: index === moonlightRow.currentIndex
                    onActivated: root.streamRequested(modelData)
                }

                Keys.onReturnPressed: {
                    if (moonlightRow.currentItem)
                        root.streamRequested(moonlightRow.currentItem.modelData)
                }
                Keys.onEnterPressed: {
                    if (moonlightRow.currentItem)
                        root.streamRequested(moonlightRow.currentItem.modelData)
                }
                Keys.onUpPressed: recentsRow.visible ? recentsRow.forceActiveFocus() : statusIcons.forceActiveFocus()
                Keys.onDownPressed: appsRow.forceActiveFocus()
                Keys.onEscapePressed: root.settingsRequested()
            }
        }

        // App view: one row per host, each card is an available app
        Repeater {
            id: appViewRepeater
            model: Theme.moonlightViewMode === "apps" ? root._appViewRows : []

            delegate: ColumnLayout {
                id: appViewRowDelegate
                required property var modelData
                required property int index

                Layout.fillWidth: true
                spacing: 8

                property var hostData: modelData
                property var hostTarget: modelData.target
                property var hostAppList: modelData.apps

                Text {
                    text: "Moonlight — " + hostData.name
                    font.pixelSize: Theme.fontTitle
                    font.bold: true
                    color: Theme.textPrimary
                }

                Item {
                    Layout.fillWidth: true
                    Layout.preferredHeight: Theme.rowHeight

                    // Offline state
                    Text {
                        visible: hostAppList.length === 0 && !root._appDiscoveryRunning
                        anchors.centerIn: parent
                        text: "Offline or no apps found"
                        font.pixelSize: Theme.fontSmall
                        color: Theme.textMuted
                    }

                    // Loading state
                    Text {
                        visible: hostAppList.length === 0 && root._appDiscoveryRunning
                        anchors.centerIn: parent
                        text: "Discovering apps..."
                        font.pixelSize: Theme.fontSmall
                        color: Theme.textMuted
                    }

                    ListView {
                        id: appViewRow
                        objectName: "appViewListView"
                        anchors.fill: parent
                        anchors.topMargin: -16
                        anchors.bottomMargin: -16
                        orientation: ListView.Horizontal
                        spacing: Theme.cardSpacing
                        clip: false
                        highlightMoveDuration: 150
                        highlightMoveVelocity: -1
                        keyNavigationEnabled: true
                        visible: hostAppList.length > 0

                        // First host row gets focus when no recents visible
                        focus: Theme.moonlightViewMode === "apps" && index === 0 && !recentsRow.visible

                        model: hostAppList

                        delegate: StreamCard {
                            required property int index
                            required property var modelData
                            height: Theme.cardHeight
                            width: Theme.cardWidth
                            target: hostTarget
                            appName: modelData
                            focus: index === appViewRow.currentIndex
                            onActivated: {
                                // Build a target with the specific app name
                                let t = JSON.parse(JSON.stringify(hostTarget))
                                t.app = modelData
                                root.streamRequested(t)
                            }
                        }

                        Keys.onReturnPressed: {
                            if (appViewRow.currentItem) {
                                let t = JSON.parse(JSON.stringify(hostTarget))
                                t.app = appViewRow.currentItem.modelData
                                root.streamRequested(t)
                            }
                        }
                        Keys.onEnterPressed: {
                            if (appViewRow.currentItem) {
                                let t = JSON.parse(JSON.stringify(hostTarget))
                                t.app = appViewRow.currentItem.modelData
                                root.streamRequested(t)
                            }
                        }

                        Keys.onUpPressed: {
                            if (appViewRowDelegate.index === 0) {
                                if (recentsRow.visible)
                                    recentsRow.forceActiveFocus()
                                else
                                    statusIcons.forceActiveFocus()
                            } else {
                                root._focusAppViewRow(appViewRowDelegate.index - 1)
                            }
                        }

                        Keys.onDownPressed: {
                            if (appViewRowDelegate.index < appViewRepeater.count - 1) {
                                root._focusAppViewRow(appViewRowDelegate.index + 1)
                            } else {
                                appsRow.forceActiveFocus()
                            }
                        }

                        Keys.onEscapePressed: root.settingsRequested()
                    }
                }
            }
        }

        // === Applications Row ===
        Text {
            text: "Applications"
            font.pixelSize: Theme.fontTitle
            font.bold: true
            color: Theme.textPrimary
        }

        Item {
            Layout.fillWidth: true
            Layout.fillHeight: true
            Layout.minimumHeight: Theme.rowHeight

            ListView {
                id: appsRow
                anchors.fill: parent
                anchors.topMargin: -16
                anchors.bottomMargin: -16
                orientation: ListView.Horizontal
                spacing: Theme.cardSpacing
                clip: false
                highlightMoveDuration: 150
                highlightMoveVelocity: -1
                keyNavigationEnabled: true

                model: root.applications

                delegate: AppCard {
                    required property int index
                    required property var modelData
                    height: Theme.cardHeight
                    width: Theme.cardWidth
                    app: modelData
                    focus: index === appsRow.currentIndex
                    onActivated: root.launchApp(modelData)
                }

                Keys.onReturnPressed: {
                    if (appsRow.currentItem)
                        root.launchApp(appsRow.currentItem.modelData)
                }
                Keys.onEnterPressed: {
                    if (appsRow.currentItem)
                        root.launchApp(appsRow.currentItem.modelData)
                }
                Keys.onUpPressed: {
                    if (Theme.moonlightViewMode === "servers") {
                        moonlightRow.forceActiveFocus()
                    } else {
                        // App view: focus the last host row
                        if (!root._focusAppViewRow(appViewRepeater.count - 1)) {
                            recentsRow.visible ? recentsRow.forceActiveFocus() : null
                        }
                    }
                }
                Keys.onEscapePressed: root.settingsRequested()
            }
        }

        // === Hint Bar ===
        Text {
            text: "A: Launch  |  B: Settings  |  ←→: Scroll  |  ↑↓: Switch Row"
            font.pixelSize: Theme.fontHint
            color: Theme.textMuted
            Layout.alignment: Qt.AlignHCenter
            Layout.bottomMargin: 16
        }
    }
}
