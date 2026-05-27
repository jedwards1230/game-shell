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

    property var runningWindows: []

    signal streamRequested(var target)
    signal streamQuitRequested(var target)
    signal appLaunchRequested(var app)
    signal appFocusRequested(string windowClass)
    signal appCloseRequested(string windowClass)
    signal settingsRequested
    signal notificationCenterRequested

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
        apps.append({'name': name, 'exec': ex.strip(), 'icon': cp.get('Desktop Entry', 'Icon', fallback=''), 'comment': cp.get('Desktop Entry', 'Comment', fallback=''), 'wmClass': cp.get('Desktop Entry', 'StartupWMClass', fallback='')})
apps.sort(key=lambda x: x['name'].lower())
print(json.dumps(apps))
`]
        stdout: SplitParser {
            onRead: line => {
                try {
                    root.applications = JSON.parse(line);
                } catch (e) {
                    console.log("Failed to parse apps:", e);
                }
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
            onRead: line => {
                try {
                    root.recentApps = JSON.parse(line);
                } catch (e) {
                    root.recentApps = [];
                }
            }
        }
    }

    Component.onCompleted: {
        loadApps.running = true;
        loadRecents.running = true;
    }

    onActiveFocusChanged: {
        if (activeFocus)
            _ensureRowFocusTimer.restart();
    }

    Timer {
        id: _ensureRowFocusTimer
        interval: 10
        onTriggered: {
            if (!root.activeFocus)
                return;
            if (statusIcons.activeFocus || runningRow.activeFocus || recentsRow.activeFocus || moonlightRow.activeFocus || appsRow.activeFocus || popoverMenu.activeFocus)
                return;
            for (let i = 0; i < appViewRepeater.count; i++) {
                let item = appViewRepeater.itemAt(i);
                if (item && item.navigableRow && item.navigableRow.activeFocus)
                    return;
            }
            root._focusFirstVisibleRow();
        }
    }

    function launchApp(app) {
        root.appLaunchRequested(app);
        recentsTracker.command = ["python3", "-c", "import json,os,time; p=os.path.expanduser('~/.local/share/game-shell/recents.json'); os.makedirs(os.path.dirname(p),exist_ok=True); " + "d=[]; " + "try:\n with open(p) as f: d=json.load(f)\nexcept: pass\n" + "entry={'name':'" + (app.name || "").replace("'", "\\'") + "','exec':'" + (app.exec || "").replace("'", "\\'") + "','comment':'" + (app.comment || "").replace("'", "\\'") + "','time':time.time()}; " + "d=[e for e in d if e.get('name')!=entry['name']]; d.insert(0,entry); d=d[:20]; " + "open(p,'w').write(json.dumps(d,indent=2))"];
        recentsTracker.running = true;
        recentsReloadTimer.start();
    }

    Process {
        id: recentsTracker
        command: ["echo"]
    }
    Timer {
        id: recentsReloadTimer
        interval: 500
        onTriggered: loadRecents.running = true
    }

    // === Moonlight App Discovery ===
    Process {
        id: appDiscovery
        property string currentHost: ""
        command: ["moonlight", "list", currentHost]
        stdout: SplitParser {
            onRead: line => {
                // moonlight list outputs lines like "1. Desktop" or just "Desktop"
                let trimmed = line.trim();
                if (trimmed === "" || trimmed.indexOf("Search") === 0 || trimmed.indexOf("Connect") === 0)
                    return;
                // Strip leading number+dot if present (e.g., "1. Desktop" -> "Desktop")
                let match = trimmed.match(/^\d+\.\s+(.+)/);
                let appName = match ? match[1] : trimmed;
                if (appName === "")
                    return;
                let updated = root.hostApps;
                if (!updated[appDiscovery.currentHost])
                    updated[appDiscovery.currentHost] = [];
                updated[appDiscovery.currentHost].push(appName);
                root.hostApps = updated;
            }
        }
        onExited: (exitCode, exitStatus) => {
            if (exitCode !== 0) {
                // Host offline or moonlight list failed — mark empty
                let updated = root.hostApps;
                updated[appDiscovery.currentHost] = [];
                root.hostApps = updated;
            }
            // Discover next host
            root._appDiscoveryIndex++;
            root._discoverNextHost();
        }
    }

    function _discoverNextHost() {
        if (_appDiscoveryIndex >= root.targets.length) {
            _appDiscoveryRunning = false;
            // Force re-evaluation by reassigning
            root.hostApps = JSON.parse(JSON.stringify(root.hostApps));
            return;
        }
        let target = root.targets[_appDiscoveryIndex];
        appDiscovery.currentHost = target.host || "";
        if (appDiscovery.currentHost === "") {
            _appDiscoveryIndex++;
            _discoverNextHost();
            return;
        }
        // Clear previous results for this host before re-query
        let updated = root.hostApps;
        updated[appDiscovery.currentHost] = [];
        root.hostApps = updated;
        appDiscovery.running = true;
    }

    function discoverAllApps() {
        if (_appDiscoveryRunning)
            return;
        _appDiscoveryRunning = true;
        _appDiscoveryIndex = 0;
        // Clear all
        root.hostApps = {};
        _discoverNextHost();
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
            discoverAllApps();
    }

    Connections {
        target: Theme
        function onMoonlightViewModeChanged() {
            if (Theme.moonlightViewMode === "apps" && root.targets.length > 0)
                root.discoverAllApps();
        }
    }

    function _appViewRowItem(idx) {
        if (idx < 0 || idx >= appViewRepeater.count)
            return null;
        var item = appViewRepeater.itemAt(idx);
        return item ? item.navigableRow : null;
    }

    function _focusFirstVisibleRow() {
        var row = runningRow;
        while (row) {
            if (row.visible) {
                row.forceActiveFocus();
                return;
            }
            row = (row.nextRow !== undefined) ? row.nextRow : null;
        }
    }

    // Computed model for app-view rows, re-evaluated when targets or hostApps change
    property var _appViewRows: {
        // Explicitly reference both properties so QML re-evaluates this binding
        let ha = root.hostApps;
        let tgts = root.targets;
        let rows = [];
        for (let i = 0; i < tgts.length; i++) {
            let t = tgts[i];
            let apps = ha[t.host] || [];
            rows.push({
                host: t.host,
                name: t.name,
                apps: apps,
                target: t
            });
        }
        return rows;
    }

    Flickable {
        id: scrollView
        anchors.fill: parent
        anchors.topMargin: Theme.padding
        anchors.bottomMargin: Theme.padding
        anchors.leftMargin: Theme.padding
        anchors.rightMargin: Theme.padding
        contentHeight: contentColumn.implicitHeight
        clip: true
        boundsBehavior: Flickable.StopAtBounds
        flickableDirection: Flickable.VerticalFlick

        function ensureVisible(item) {
            if (!item)
                return;
            let mapped = item.mapToItem(contentColumn, 0, 0);
            let itemTop = mapped.y;
            let itemBottom = itemTop + item.height;
            if (itemTop < contentY)
                contentY = Math.max(0, itemTop - 24);
            else if (itemBottom > contentY + height)
                contentY = Math.min(contentHeight - height, itemBottom - height + 24);
        }

        Behavior on contentY {
            NumberAnimation {
                duration: 300
                easing.type: Easing.OutCubic
            }
        }

        ColumnLayout {
            id: contentColumn
            width: scrollView.width
            spacing: 24

            // === Hero Clock Area ===
            RowLayout {
                Layout.fillWidth: true
                Layout.preferredHeight: Units.gridUnit * 9
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
                                let now = new Date();
                                heroClockText.text = now.toLocaleTimeString(Qt.locale(), "h:mm AP");
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
                                let now = new Date();
                                heroDateText.text = now.toLocaleDateString(Qt.locale(), "dddd, MMMM d");
                            }
                        }
                    }
                }

                Item {
                    Layout.fillWidth: true
                }

                // Status icons (right side)
                StatusIcons {
                    id: statusIcons
                    Layout.alignment: Qt.AlignTop | Qt.AlignRight
                    onSettingsRequested: root.settingsRequested()
                    onNotificationCenterRequested: root.notificationCenterRequested()
                    onFocusDownRequested: root._focusFirstVisibleRow()
                    onActiveFocusChanged: if (activeFocus)
                        scrollView.contentY = 0
                }
            }

            // === Running Windows Row ===
            Text {
                visible: root.runningWindows.length > 0
                text: "Running"
                font.pixelSize: Theme.fontTitle
                font.bold: true
                color: Theme.textPrimary
            }

            NavigableRow {
                id: runningRow
                visible: root.runningWindows.length > 0
                Layout.fillWidth: true
                Layout.preferredHeight: visible ? Theme.rowHeight : 0
                keyNavigationWraps: true
                focus: visible
                previousRow: statusIcons
                nextRow: recentsRow
                model: root.runningWindows
                onActiveFocusChanged: if (activeFocus)
                    scrollView.ensureVisible(this)

                delegate: AppCard {
                    required property int index
                    required property var modelData
                    height: Theme.cardHeight
                    width: Theme.cardWidth
                    app: modelData
                    focus: index === runningRow.currentIndex
                    onActivated: root.appFocusRequested(modelData.windowClass)
                }

                onContextRequested: {
                    if (currentItem && currentIndex >= 0 && currentIndex < root.runningWindows.length) {
                        let pos = currentItem.mapToItem(root, currentItem.width / 2, 0);
                        popoverMenu.targetX = pos.x;
                        popoverMenu.targetY = pos.y;
                        let wc = root.runningWindows[currentIndex].windowClass;
                        popoverMenu.actions = [
                            {
                                label: "Resume",
                                action: function () {
                                    root.appFocusRequested(wc);
                                }
                            },
                            {
                                label: "Quit App",
                                action: function () {
                                    root.appCloseRequested(wc);
                                }
                            }
                        ];
                        popoverMenu.opened = true;
                        popoverMenu.forceActiveFocus();
                    }
                }
                onEscaped: root.settingsRequested()
            }

            // === Recents Row ===
            Text {
                visible: root.recentApps.length > 0
                text: "Recent"
                font.pixelSize: Theme.fontTitle
                font.bold: true
                color: Theme.textPrimary
            }

            NavigableRow {
                id: recentsRow
                visible: root.recentApps.length > 0
                Layout.fillWidth: true
                Layout.preferredHeight: visible ? Theme.rowHeight : 0
                keyNavigationWraps: true
                focus: visible && !runningRow.visible
                previousRow: runningRow
                onActiveFocusChanged: if (activeFocus)
                    scrollView.ensureVisible(this)
                nextRow: {
                    var _ = appViewRepeater.count;
                    if (Theme.moonlightViewMode === "servers")
                        return moonlightRow;
                    return root._appViewRowItem(0) || appsRow;
                }
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

                onEscaped: root.settingsRequested()
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

            NavigableRow {
                id: moonlightRow
                visible: Theme.moonlightViewMode === "servers"
                Layout.fillWidth: true
                Layout.preferredHeight: visible ? Theme.rowHeight : 0
                keyNavigationWraps: true
                focus: Theme.moonlightViewMode === "servers" && !recentsRow.visible && !runningRow.visible
                previousRow: recentsRow
                nextRow: appsRow
                onActiveFocusChanged: if (activeFocus)
                    scrollView.ensureVisible(this)
                model: root.targets

                delegate: StreamCard {
                    required property int index
                    required property var modelData
                    height: Theme.cardHeight
                    width: Theme.cardWidth
                    target: modelData
                    shellState: root.shellState
                    focus: index === moonlightRow.currentIndex
                    onActivated: root.streamRequested(modelData)
                }

                onContextRequested: {
                    if (currentItem && currentIndex >= 0 && currentIndex < root.targets.length) {
                        let card = currentItem;
                        let target = root.targets[currentIndex];
                        if (card.hasActiveSession) {
                            let pos = card.mapToItem(root, card.width / 2, 0);
                            popoverMenu.targetX = pos.x;
                            popoverMenu.targetY = pos.y;
                            popoverMenu.actions = [
                                {
                                    label: "Resume",
                                    action: function () {
                                        root.streamRequested(target);
                                    }
                                },
                                {
                                    label: "Quit Stream",
                                    action: function () {
                                        root.streamQuitRequested(target);
                                    }
                                }
                            ];
                            popoverMenu.opened = true;
                            popoverMenu.forceActiveFocus();
                        }
                    }
                }
                onEscaped: root.settingsRequested()
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
                    spacing: contentColumn.spacing

                    property var hostData: modelData
                    property var hostTarget: modelData.target
                    property var hostAppList: modelData.apps
                    property alias navigableRow: appViewNavRow

                    RowLayout {
                        spacing: 12

                        Text {
                            text: "Moonlight — " + hostData.name
                            font.pixelSize: Theme.fontTitle
                            font.bold: true
                            color: Theme.textPrimary
                        }

                        Rectangle {
                            width: 14
                            height: 14
                            radius: 7
                            color: hostAppList.length > 0 ? Theme.online : Theme.offline
                        }
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

                        NavigableRow {
                            id: appViewNavRow
                            anchors.fill: parent
                            visible: hostAppList.length > 0
                            keyNavigationWraps: true
                            focus: Theme.moonlightViewMode === "apps" && appViewRowDelegate.index === 0 && !recentsRow.visible && !runningRow.visible
                            onActiveFocusChanged: if (activeFocus)
                                scrollView.ensureVisible(appViewRowDelegate)
                            model: hostAppList
                            previousRow: {
                                var _ = appViewRepeater.count;
                                return appViewRowDelegate.index === 0 ? recentsRow : root._appViewRowItem(appViewRowDelegate.index - 1);
                            }
                            nextRow: appViewRowDelegate.index < appViewRepeater.count - 1 ? root._appViewRowItem(appViewRowDelegate.index + 1) : appsRow

                            delegate: StreamCard {
                                required property int index
                                required property var modelData
                                height: Theme.cardHeight
                                width: Theme.cardWidth
                                target: hostTarget
                                appName: modelData
                                shellState: root.shellState
                                focus: index === appViewNavRow.currentIndex
                                onActivated: {
                                    let t = JSON.parse(JSON.stringify(hostTarget));
                                    t.app = modelData;
                                    root.streamRequested(t);
                                }
                            }

                            onContextRequested: {
                                if (currentItem && currentIndex >= 0 && currentIndex < hostAppList.length) {
                                    let card = currentItem;
                                    if (card.hasActiveSession) {
                                        let pos = card.mapToItem(root, card.width / 2, 0);
                                        popoverMenu.targetX = pos.x;
                                        popoverMenu.targetY = pos.y;
                                        let t = JSON.parse(JSON.stringify(hostTarget));
                                        t.app = hostAppList[currentIndex];
                                        popoverMenu.actions = [
                                            {
                                                label: "Resume",
                                                action: function () {
                                                    root.streamRequested(t);
                                                }
                                            },
                                            {
                                                label: "Quit Stream",
                                                action: function () {
                                                    root.streamQuitRequested(t);
                                                }
                                            }
                                        ];
                                        popoverMenu.opened = true;
                                        popoverMenu.forceActiveFocus();
                                    }
                                }
                            }
                            onEscaped: root.settingsRequested()
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

            NavigableRow {
                id: appsRow
                Layout.fillWidth: true
                Layout.preferredHeight: Theme.rowHeight
                keyNavigationWraps: true
                onActiveFocusChanged: if (activeFocus)
                    scrollView.ensureVisible(this)
                previousRow: {
                    if (Theme.moonlightViewMode === "servers")
                        return moonlightRow;
                    return root._appViewRowItem(appViewRepeater.count - 1) || recentsRow;
                }
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

                onEscaped: root.settingsRequested()
            }

            // === Hint Bar ===
            Text {
                text: runningRow.activeFocus ? "A: Resume  |  Y: Actions  |  B: Settings  |  ←→: Scroll  |  ↑↓: Switch Row" : (moonlightRow.activeFocus ? "A: Stream  |  Y: Actions  |  B: Settings  |  ←→: Scroll  |  ↑↓: Switch Row" : "A: Launch  |  B: Settings  |  ←→: Scroll  |  ↑↓: Switch Row")
                font.pixelSize: Theme.fontHint
                color: Theme.textMuted
                Layout.alignment: Qt.AlignHCenter
                Layout.bottomMargin: 16
            }
        }
    }

    PopoverMenu {
        id: popoverMenu
        onClosed: {
            popoverMenu.opened = false;
            runningRow.forceActiveFocus();
        }
    }
}
