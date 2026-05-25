pragma Singleton
import Quickshell.Io
import QtQuick

Item {
    // === Theme Mode ===
    // "auto" (time-based), "light", "dark"
    property string themeMode: "dark"
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
                } catch(e) { console.log("Theme: failed to parse settings:", e) }
            }
        }
    }

    Process {
        id: saveSettings
        command: ["bash", "-c",
            "mkdir -p " + _settingsDir + " && " +
            "echo '{\"themeMode\":\"" + themeMode + "\"}' > " + _settingsFile]
    }

    function setThemeMode(mode) {
        if (mode === "auto" || mode === "light" || mode === "dark") {
            themeMode = mode
            saveSettings.running = true
        }
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
    property color background: darkMode ? "#0f1623" : snow
    property color surface: darkMode ? "#1a2a42" : "#ffffff"
    property color surfaceHover: darkMode ? "#243550" : "#ecedf0"
    property color surfaceBorder: darkMode ? "#2d3f5a" : "#dcdee3"

    // Text hierarchy
    property color textPrimary: darkMode ? "#e8eaf0" : "#1a2540"
    property color textSecondary: darkMode ? "#8899b0" : "#4a5568"
    property color textMuted: darkMode ? "#556680" : "#8892a4"
    property color textOnDark: "#f4f5f7"
    property color textOnDarkMuted: "#c8ccd4"

    // Cards
    property color cardBackground: darkMode ? "#162033" : "#ffffff"
    property color cardAccent: ember

    // Status
    readonly property color online: "#2d8a4e"
    property color offline: crimson
    property color warning: ember

    // Interactive
    property color focusBorder: darkMode ? "#4a9eff" : crimson
    property color focusGlow: darkMode ? "#4a9eff33" : "#c7213833"
    property color barBackground: darkMode ? "#0a1020" : navy
    property color sidebarActive: darkMode ? "#1e3a5f" : navy
    property color sidebarText: "#e8eaf0"

    // === Layout — couch-readable at 4K (10-foot UI) ===
    readonly property int cardWidth: 640
    readonly property int cardHeight: 360
    readonly property int cardSpacing: 48
    readonly property int cardRadius: 24
    readonly property int statusBarHeight: 120
    readonly property int padding: 48
    readonly property int rowHeight: 420

    // === Font sizes — couch-readable at 4K ===
    readonly property int fontHero: 72
    readonly property int fontTitle: 56
    readonly property int fontBody: 40
    readonly property int fontSmall: 32
    readonly property int fontStatus: 40
    readonly property int fontHint: 36
}
