pragma Singleton
import Quickshell.Io
import QtQuick

Item {
    // === Theme Mode ===
    // "auto" (time-based), "light", "dark"
    property string themeMode: "dark"

    // === Moonlight View Mode ===
    // "servers" (one card per host) or "apps" (one row per host, cards = apps)
    property string moonlightViewMode: "servers"

    // === Controller Debug Overlay ===
    property bool controllerDebug: false
    property int _currentHour: new Date().getHours()
    property bool darkMode: {
        if (themeMode === "dark") return true
        if (themeMode === "light") return false
        // auto: dark from 8 PM to 7 AM
        return _currentHour >= 20 || _currentHour < 7
    }

    // === Settings Persistence ===
    readonly property string _settingsDir: "~/.config/game-shell"
    readonly property string _settingsFile: _settingsDir + "/settings.json"

    Process {
        id: loadSettings
        command: ["bash", "-c", "cat " + _settingsFile + " 2>/dev/null || true"]
        stdout: SplitParser {
            onRead: (line) => {
                try {
                    var obj = JSON.parse(line)
                    if (obj.themeMode === "auto" || obj.themeMode === "light" || obj.themeMode === "dark")
                        themeMode = obj.themeMode
                    if (obj.moonlightViewMode === "servers" || obj.moonlightViewMode === "apps")
                        moonlightViewMode = obj.moonlightViewMode
                    if (typeof obj.controllerDebug === "boolean")
                        controllerDebug = obj.controllerDebug
                } catch(e) { console.log("Theme: failed to parse settings:", e) }
            }
        }
    }

    // NOTE: Both Theme.qml and the input daemon do read-modify-write on
    // settings.json without file locking.  This is acceptable for a single-user
    // kiosk — the two writers update disjoint keys and rarely race in practice.
    Process {
        id: saveSettings
        command: ["python3", "-c",
            "import json,os,pathlib;" +
            "p=pathlib.Path(os.path.expanduser('" + _settingsFile + "'));" +
            "p.parent.mkdir(parents=True,exist_ok=True);" +
            "d=json.loads(p.read_text()) if p.exists() else {};" +
            "d['themeMode']='" + themeMode + "';" +
            "d['moonlightViewMode']='" + moonlightViewMode + "';" +
            "d['controllerDebug']=" + (controllerDebug ? "True" : "False") + ";" +
            "p.write_text(json.dumps(d,separators=(',',':')))"]
    }

    function setThemeMode(mode) {
        if (mode === "auto" || mode === "light" || mode === "dark") {
            themeMode = mode
            saveSettings.running = true
        }
    }

    function setMoonlightViewMode(mode) {
        if (mode === "servers" || mode === "apps") {
            moonlightViewMode = mode
            saveSettings.running = true
        }
    }

    function setControllerDebug(enabled) {
        controllerDebug = enabled
        saveSettings.running = true
    }

    // Re-evaluate auto mode every 60 seconds
    Timer {
        id: autoTimer
        interval: 60000
        running: themeMode === "auto"
        repeat: true
        onTriggered: { _currentHour = new Date().getHours() }
    }

    Component.onCompleted: { loadSettings.running = true }

    // === Base Palette (5 accent colors — shared across themes) ===
    readonly property color snow: "#f4f5f7"
    readonly property color crimson: "#c72138"
    readonly property color ember: "#e06236"
    readonly property color gold: "#d7a64b"
    readonly property color navy: "#304c7a"

    // === Semantic Colors (switch on darkMode) ===
    property color background: darkMode ? "#111215" : snow
    property color surface: darkMode ? "#33363f" : "#ffffff"
    property color surfaceHover: darkMode ? "#424650" : "#ecedf0"
    property color surfaceBorder: darkMode ? "#4d525c" : "#dcdee3"

    // Text hierarchy
    property color textPrimary: darkMode ? "#e6e4e0" : "#1a2540"
    property color textSecondary: darkMode ? "#c2bfba" : "#4a5568"
    property color textMuted: darkMode ? "#928e88" : "#8892a4"
    property color textOnDark: "#f4f5f7"
    property color textOnDarkMuted: "#d8d5d0"

    // Cards
    property color cardBackground: darkMode ? "#2e3139" : "#ffffff"
    property color cardAccent: ember

    // Status
    readonly property color online: "#2d8a4e"
    property color offline: crimson
    property color warning: ember

    // Interactive
    property color focusBorder: crimson
    property color focusGlow: "#c7213833"
    property color barBackground: darkMode ? "#111215" : navy
    property color sidebarActive: darkMode ? "#424650" : navy
    property color sidebarText: "#e6e4e0"

    // === Layout — couch-readable at 4K (10-foot UI) ===
    readonly property int cardWidth: 600
    readonly property int cardHeight: 480
    readonly property int cardSpacing: 40
    readonly property int cardRadius: 24
    readonly property int padding: 48
    readonly property int rowHeight: 540
    readonly property int statusBarHeight: 120

    // === Font sizes — couch-readable at 4K ===
    readonly property int fontHero: 120
    readonly property int fontTitle: 56
    readonly property int fontBody: 40
    readonly property int fontSmall: 32
    readonly property int fontStatus: 40
    readonly property int fontHint: 36
}
