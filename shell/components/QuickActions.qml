import QtQuick
import "lib"

// Horizontal row of quick-action icons, navigable left-to-right. Reused in
// the top-right status strip (HomeScreen) and the navigation drawer. Icons
// are laid out left→right (index 0 = leftmost); Power is the last/rightmost
// action. Overflowing icons scroll horizontally and the selected icon is
// always kept in view, so the row stays usable as actions are added.
FocusScope {
    id: root

    signal settingsRequested
    signal widgetsRequested
    signal notificationCenterRequested
    signal powerRequested
    // Network/Volume carry the originating glyph's scene-root rect so the
    // overlay can anchor itself as a popover next to the glyph (#118).
    signal networkRequested(var anchorRect)
    signal volumeRequested(var anchorRect)
    signal focusDownRequested
    signal focusUpRequested

    property int currentIndex: 0
    property int iconSize: 76
    property int imgSize: 46
    readonly property int _spacing: 12
    // Cap the visible width to enable the scroll carousel. Defaults to
    // effectively unbounded (top-right has room for all icons).
    property int maxContentWidth: 100000
    // HomeScreen keeps the legacy "Escape opens Settings" affordance; the
    // drawer sets this false so Escape/B bubbles up to close the drawer.
    property bool escapeRequestsSettings: true

    // Must match the number of icon containers below
    // (Notifications=0, Settings=1, Widgets=2, Theme=3, Network=4, Volume=5, Power=6)
    readonly property int _iconCount: 7
    readonly property var _labels: ["Notifications", "Settings", "Widgets", "Theme", "Network", "Volume", "Power"]
    property int _labelHeight: Theme.fontHint + Units.spacingSM

    implicitWidth: Math.min(iconRow.implicitWidth, maxContentWidth)
    implicitHeight: iconSize + _labelHeight

    function _ensureVisible() {
        var left = currentIndex * (iconSize + _spacing);
        var right = left + iconSize;
        if (left < flick.contentX)
            flick.contentX = left;
        else if (right > flick.contentX + flick.width)
            flick.contentX = right - flick.width;
    }

    onCurrentIndexChanged: _ensureVisible()
    onWidthChanged: _ensureVisible()

    // Map a glyph item's rect to scene-root coords (mapToItem(null, ...)),
    // returned as {x, y, w, h}. Overlays use this to anchor a popover beside
    // the originating glyph wherever the QuickActions row lives (#118).
    function _glyphRect(item) {
        if (!item)
            return {
                x: 0,
                y: 0,
                w: 0,
                h: 0
            };
        var p = item.mapToItem(null, 0, 0);
        return {
            x: p.x,
            y: p.y,
            w: item.width,
            h: item.height
        };
    }

    // Keyboard navigation (LTR: Left lowers index, Right raises it). Any nav
    // key means controller/keyboard is driving — flip out of mouse-mode (#45),
    // no daemon round-trip.
    Keys.onLeftPressed: {
        InputMode.exitMouseMode();
        if (currentIndex > 0)
            currentIndex--;
    }
    Keys.onRightPressed: {
        InputMode.exitMouseMode();
        if (currentIndex < _iconCount - 1)
            currentIndex++;
    }
    Keys.onDownPressed: {
        InputMode.exitMouseMode();
        root.focusDownRequested();
    }
    Keys.onUpPressed: {
        InputMode.exitMouseMode();
        root.focusUpRequested();
    }
    Keys.onEscapePressed: event => {
        InputMode.exitMouseMode();
        if (root.escapeRequestsSettings) {
            root.settingsRequested();
        } else {
            // Drawer context: don't consume — let Escape bubble up to
            // Drawer.qml's handler so B/Escape closes the drawer (#142).
            event.accepted = false;
        }
    }
    Keys.onReturnPressed: {
        InputMode.exitMouseMode();
        root._activate(currentIndex);
    }

    function _activate(index) {
        switch (index) {
        case 0:
            root.notificationCenterRequested();
            break;
        case 1:
            root.settingsRequested();
            break;
        case 2:
            root.widgetsRequested();
            break;
        case 3:
            if (Theme.themeMode === "auto")
                SettingsStore.setThemeMode("light");
            else if (Theme.themeMode === "light")
                SettingsStore.setThemeMode("dark");
            else
                SettingsStore.setThemeMode("auto");
            break;
        case 4:
            root.networkRequested(root._glyphRect(netGlyph));
            break;
        case 5:
            root.volumeRequested(root._glyphRect(volGlyph));
            break;
        case 6:
            root.powerRequested();
            break;
        }
    }

    Connections {
        target: Theme
        function onMouseModeChanged() {
            if (InputMode.mouseMode)
                return;
            if (notifGlyph.hovered) {
                root.currentIndex = 0;
                root.forceActiveFocus();
            } else if (settingsGlyph.hovered) {
                root.currentIndex = 1;
                root.forceActiveFocus();
            } else if (widgetsGlyph.hovered) {
                root.currentIndex = 2;
                root.forceActiveFocus();
            } else if (themeGlyph.hovered) {
                root.currentIndex = 3;
                root.forceActiveFocus();
            } else if (netGlyph.hovered) {
                root.currentIndex = 4;
                root.forceActiveFocus();
            } else if (volGlyph.hovered) {
                root.currentIndex = 5;
                root.forceActiveFocus();
            } else if (powerGlyph.hovered) {
                root.currentIndex = 6;
                root.forceActiveFocus();
            }
        }
    }

    Flickable {
        id: flick
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.top: parent.top
        height: root.iconSize
        contentWidth: iconRow.width
        contentHeight: height
        clip: true
        interactive: false
        boundsBehavior: Flickable.StopAtBounds

        Behavior on contentX {
            NumberAnimation {
                duration: 150
                easing.type: Easing.OutCubic
            }
        }

        Row {
            id: iconRow
            height: parent.height
            spacing: root._spacing

            // Notifications (index 0)
            QuickActionButton {
                id: notifGlyph
                index: 0
                currentIndex: root.currentIndex
                rowActiveFocus: root.activeFocus
                iconSize: root.iconSize
                imgSize: root.imgSize
                iconPath: IconTheme.base ? "file://" + IconTheme.base + "/actions/22/" + (NotificationManager.unreadCount > 0 ? "notification-active.svg" : "notification-inactive.svg") : ""
                fallbackGlyph: "\u{1F514}"
                fallbackColor: notifGlyph.hovered && InputMode.mouseMode ? Theme.textPrimary : Theme.textMuted
                a11yName: root._labels[0]
                onActivated: root.notificationCenterRequested()

                CountBadge {
                    count: NotificationManager.unreadCount
                    anchors.top: parent.top
                    anchors.right: parent.right
                    anchors.topMargin: 6
                    anchors.rightMargin: 6
                }
            }

            // Settings (index 1)
            QuickActionButton {
                id: settingsGlyph
                index: 1
                currentIndex: root.currentIndex
                rowActiveFocus: root.activeFocus
                iconSize: root.iconSize
                imgSize: root.imgSize
                iconPath: IconTheme.base ? "file://" + IconTheme.base + "/actions/22/configure.svg" : ""
                fallbackGlyph: "⚙"
                fallbackColor: settingsGlyph.hovered && InputMode.mouseMode ? Theme.textPrimary : Theme.textMuted
                a11yName: root._labels[1]
                onActivated: root.settingsRequested()
            }

            // Widgets (index 2)
            // No freedesktop "widgets" action icon exists, and the target host has
            // no system icon theme anyway — so this is glyph-only (iconPath: ""),
            // exactly like the Theme toggle. Use ⊞ (U+229E SQUARED PLUS) — a
            // square split into four tiles, the "grid-view / app-tiles" reading
            // for Widgets. It's a Mathematical Operators glyph (same family the
            // sibling symbol glyphs ⚙/♫/⏻ come from), so it renders at a
            // consistent weight — unlike the old ▦ (U+25A6), a Geometric-Shapes
            // box that the font draws undersized next to the others. Vertical
            // centering is computed from font metrics in QuickActionButton, so
            // this needs no per-glyph nudge.
            QuickActionButton {
                id: widgetsGlyph
                index: 2
                currentIndex: root.currentIndex
                rowActiveFocus: root.activeFocus
                iconSize: root.iconSize
                imgSize: root.imgSize
                iconPath: ""
                fallbackGlyph: "\u{229E}"
                fallbackColor: widgetsGlyph.hovered && InputMode.mouseMode ? Theme.textPrimary : Theme.textMuted
                a11yName: root._labels[2]
                onActivated: root.widgetsRequested()
            }

            // Theme toggle (index 3)
            // Simple monochrome glyph — intentionally colorless so it
            // reads the same in light and dark themes (no colored
            // weather artwork). Adapts to the theme text color.
            QuickActionButton {
                id: themeGlyph
                index: 3
                currentIndex: root.currentIndex
                rowActiveFocus: root.activeFocus
                iconSize: root.iconSize
                imgSize: root.imgSize
                iconPath: ""
                fallbackGlyph: Theme.themeMode === "dark" ? "☾" : Theme.themeMode === "light" ? "☀" : "◐"
                fallbackColor: Theme.textPrimary
                a11yName: root._labels[3]
                onActivated: {
                    if (Theme.themeMode === "auto")
                        SettingsStore.setThemeMode("light");
                    else if (Theme.themeMode === "light")
                        SettingsStore.setThemeMode("dark");
                    else
                        SettingsStore.setThemeMode("auto");
                }
            }

            // Network (index 4)
            QuickActionButton {
                id: netGlyph
                index: 4
                currentIndex: root.currentIndex
                rowActiveFocus: root.activeFocus
                iconSize: root.iconSize
                imgSize: root.imgSize
                iconPath: {
                    if (!IconTheme.base)
                        return "";
                    if (NetworkManager.connected)
                        return "file://" + IconTheme.base + "/status/22/network-wired.svg";
                    return "file://" + IconTheme.base + "/actions/22/network-disconnect.svg";
                }
                fallbackGlyph: NetworkManager.connected ? "⇅" : "⚠"
                fallbackColor: NetworkManager.connected ? Theme.textMuted : Theme.warning
                a11yName: root._labels[4]
                onActivated: root.networkRequested(root._glyphRect(netGlyph))
            }

            // Volume (index 5)
            QuickActionButton {
                id: volGlyph
                index: 5
                currentIndex: root.currentIndex
                rowActiveFocus: root.activeFocus
                iconSize: root.iconSize
                imgSize: root.imgSize
                iconPath: IconTheme.base ? "file://" + IconTheme.base + "/status/22/audio-volume-high.svg" : ""
                fallbackGlyph: "♫"
                fallbackColor: Theme.textMuted
                a11yName: root._labels[5]
                onActivated: root.volumeRequested(root._glyphRect(volGlyph))
            }

            // Power (index 6)
            QuickActionButton {
                id: powerGlyph
                index: 6
                currentIndex: root.currentIndex
                rowActiveFocus: root.activeFocus
                iconSize: root.iconSize
                imgSize: root.imgSize
                iconPath: IconTheme.base ? "file://" + IconTheme.base + "/actions/22/system-shutdown.svg" : ""
                fallbackGlyph: "⏻"
                fallbackColor: powerGlyph.hovered && InputMode.mouseMode ? Theme.textPrimary : Theme.textMuted
                a11yName: root._labels[6]
                onActivated: root.powerRequested()
            }
        }
    }

    Text {
        id: actionLabel
        anchors.top: flick.bottom
        anchors.topMargin: Units.spacingSM
        anchors.horizontalCenter: parent.horizontalCenter
        text: {
            if (InputMode.mouseMode) {
                if (notifGlyph.hovered)
                    return root._labels[0];
                if (settingsGlyph.hovered)
                    return root._labels[1];
                if (widgetsGlyph.hovered)
                    return root._labels[2];
                if (themeGlyph.hovered)
                    return root._labels[3];
                if (netGlyph.hovered)
                    return root._labels[4];
                if (volGlyph.hovered)
                    return root._labels[5];
                if (powerGlyph.hovered)
                    return root._labels[6];
                return "";
            }
            return (root.activeFocus && root.currentIndex >= 0 && root.currentIndex < root._labels.length) ? root._labels[root.currentIndex] : "";
        }
        visible: text.length > 0
        font.pixelSize: Theme.fontHint
        color: Theme.textMuted
    }
}
