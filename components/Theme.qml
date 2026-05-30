pragma Singleton
import QtQuick

Item {
    // === Theme Mode (persisted via SettingsStore) ===
    // "auto" (time-based), "light", "dark"
    readonly property string themeMode: SettingsStore.themeMode

    // === Moonlight View Mode (persisted via SettingsStore) ===
    // "servers" (one card per host) or "apps" (one row per host, cards = apps)
    readonly property string streamingViewMode: SettingsStore.streamingViewMode

    // === Input Mode ===
    // true when mouse/right-stick is driving focus, false for controller/D-pad.
    //
    // Multi-source (#45): this UI flag is owned by QML, which observes its own
    // Wayland pointer and key/gamepad-nav events directly — a physical K400
    // mouse never reaches the daemon, so hover/move must flip mouse-mode WITHOUT
    // any daemon round-trip. The daemon's `input-mode:*` event (InputManager)
    // is now just ONE more source, covering only its right-stick->cursor case.
    // Funnel writes through enterMouseMode()/exitMouseMode() so redundant sets
    // don't emit spurious mouseModeChanged() (every consumer re-syncs focus on
    // that signal).
    property bool mouseMode: false

    // A real Wayland pointer event (hover/move/click) — switch to mouse mode.
    function enterMouseMode() {
        if (!mouseMode)
            mouseMode = true;
    }

    // A key or gamepad-nav event — switch back to controller mode.
    function exitMouseMode() {
        if (mouseMode)
            mouseMode = false;
    }

    // === Controller Debug Overlay (persisted via SettingsStore) ===
    readonly property bool controllerDebug: SettingsStore.controllerDebug
    property int _currentHour: new Date().getHours()
    property bool darkMode: {
        if (themeMode === "dark")
            return true;
        if (themeMode === "light")
            return false;
        // auto: dark from 8 PM to 7 AM
        return _currentHour >= 20 || _currentHour < 7;
    }

    // === Settings Persistence ===
    // All settings I/O is centralized in SettingsStore; Theme delegates to it
    // and exposes the values via the read-through properties above.
    function setThemeMode(mode) {
        SettingsStore.setThemeMode(mode);
    }

    function setStreamingViewMode(mode) {
        SettingsStore.setStreamingViewMode(mode);
    }

    function setControllerDebug(enabled) {
        SettingsStore.setControllerDebug(enabled);
    }

    // Re-evaluate auto mode every 60 seconds
    Timer {
        id: autoTimer
        interval: 60000
        running: themeMode === "auto"
        repeat: true
        onTriggered: {
            _currentHour = new Date().getHours();
        }
    }

    // === Base Palette (5 accent colors — shared across themes) ===
    readonly property color snow: "#f4f5f7"
    readonly property color crimson: "#c72138"
    readonly property color ember: "#e06236"
    readonly property color gold: "#d7a64b"
    readonly property color navy: "#304c7a"

    // === Structured Palette Objects ===
    // All theme-dependent colors grouped per mode. Adding a new theme =
    // one new object instead of editing 12 ternaries.
    readonly property var _darkPalette: ({
            background: "#111215",
            surface: "#33363f",
            surfaceHover: "#424650",
            surfaceBorder: "#4d525c",
            textPrimary: "#e6e4e0",
            textSecondary: "#c2bfba",
            textMuted: "#928e88",
            cardBackground: "#2e3139",
            focusBorder: String(crimson),
            focusGlow: String(crimson) + "33",
            barBackground: "#111215",
            sidebarActive: "#424650"
        })
    readonly property var _lightPalette: ({
            background: String(snow),
            surface: "#ffffff",
            surfaceHover: "#ecedf0",
            surfaceBorder: "#dcdee3",
            textPrimary: "#1a2540",
            textSecondary: "#4a5568",
            textMuted: "#8892a4",
            cardBackground: "#ffffff",
            focusBorder: String(crimson),
            focusGlow: String(crimson) + "33",
            barBackground: String(navy),
            sidebarActive: String(navy)
        })
    readonly property var palette: darkMode ? _darkPalette : _lightPalette

    // === Semantic Colors (aliases into active palette — no breaking changes) ===
    property color background: palette.background
    property color surface: palette.surface
    property color surfaceHover: palette.surfaceHover
    property color surfaceBorder: palette.surfaceBorder

    // Text hierarchy
    property color textPrimary: palette.textPrimary
    property color textSecondary: palette.textSecondary
    property color textMuted: palette.textMuted
    property color textOnDark: "#f4f5f7"
    property color textOnDarkMuted: "#d8d5d0"

    // Cards
    property color cardBackground: palette.cardBackground
    property color cardAccent: ember

    // Status
    readonly property color online: "#2d8a4e"
    property color offline: crimson
    property color warning: ember

    // Interactive
    property color focusBorder: palette.focusBorder
    property color focusGlow: palette.focusGlow
    property color barBackground: palette.barBackground
    property color sidebarActive: palette.sidebarActive
    property color sidebarText: "#e6e4e0"

    // === Layout — derived from Units.gridUnit (couch-readable, resolution-adaptive) ===
    readonly property int cardWidth: Math.round(Units.gridUnit * 11.11)
    readonly property int cardHeight: Math.round(Units.gridUnit * 8.89)
    readonly property int cardSpacing: Math.round(Units.gridUnit * 0.74)
    readonly property int cardRadius: Math.round(Units.gridUnit * 0.44)
    readonly property int padding: Math.round(Units.gridUnit * 0.89)
    readonly property int rowHeight: Math.round(Units.gridUnit * 10.0)
    readonly property int statusBarHeight: Math.round(Units.gridUnit * 2.22)

    // === Font sizes — derived from Units.gridUnit (couch-readable, resolution-adaptive) ===
    readonly property int fontHero: Math.round(Units.gridUnit * 2.22)
    readonly property int fontTitle: Math.round(Units.gridUnit * 1.04)
    readonly property int fontBody: Math.round(Units.gridUnit * 0.74)
    readonly property int fontSmall: Math.round(Units.gridUnit * 0.59)
    readonly property int fontStatus: Math.round(Units.gridUnit * 0.74)
    readonly property int fontHint: Math.round(Units.gridUnit * 0.67)
    readonly property int fontCaption: Math.round(Units.gridUnit * 0.52)
    readonly property int fontXSmall: Math.round(Units.gridUnit * 0.44)
}
