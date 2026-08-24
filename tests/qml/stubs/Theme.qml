pragma Singleton
import QtQuick

// Test stub for the production `Theme` singleton (shell/components/Theme.qml).
// QtQuickTest runs headless under QT_QPA_PLATFORM=offscreen with NO Quickshell
// runtime, so the real Theme — which reads SettingsStore (Quickshell.Io) and
// InputMode — can't load. This stub reproduces ONLY the surface consumed by the
// components under test (QuickActions / QuickActionButton / CountBadge). Keep it
// in sync with the real API as the tested component set grows; see
// tests/qml/README.md.
Item {
    // Mirror the real delegation so the theme-toggle path is exercised end to
    // end: Theme.themeMode reflects SettingsStore, and toggling SettingsStore
    // flips it back here.
    readonly property string themeMode: SettingsStore.themeMode
    readonly property bool mouseMode: InputMode.mouseMode

    property int fontHint: 18

    // Accessibility flag read by MarqueeText (real one comes from SettingsStore).
    property bool reduceMotion: false

    // Card geometry — NavigableGrid reads these as the default cell footprint
    // (overridable per-instance). Fixed values keep grid-layout math deterministic.
    property int cardWidth: 200
    property int cardHeight: 120
    property int cardSpacing: 16
    // Panel geometry read by PopoverMenu (radius + edge clamp).
    property int cardRadius: 14
    property int padding: 28

    // Row/font metrics the widget-contract components bind (tst_widgetcontract).
    property int rowHeight: 140
    property int fontTitle: 28
    property int fontBody: 20
    property int fontSmall: 16
    property int fontCaption: 14

    property color textPrimary: "#ffffff"
    property color textSecondary: "#c0c0c0"
    property color textMuted: "#9aa0a6"
    property color warning: "#e0a030"
    property color surface: "#141414"
    property color surfaceHover: "#202020"
    property color surfaceBorder: "#303030"
    property color sidebarActive: "#dd3333"
    property color focusBorder: "#dd3333"
    property color crimson: "#dd3333"
    property color ember: "#e0662e"
    property color gold: "#d4af37"
    property color offline: "#888888"
    property color textOnDark: "#ffffff"
}
