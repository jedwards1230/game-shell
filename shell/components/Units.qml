pragma Singleton
import Quickshell
import QtQuick
import "screenScale.js" as ScreenScale

Item {
    id: units

    // Single-screen kiosk — every size below scales off the primary screen's
    // height, so this one number decides the whole UI's geometry.
    //
    // The filter itself (what counts as a valid reading, what to hold when a
    // reading is degenerate, and when the scale becomes trustworthy) lives in the
    // pure, headless-tested `screenScale.js`; this file only wires it to
    // Quickshell. Read that module's header for the why — in short, a transiently
    // EMPTY `Quickshell.screens` or a ~0-height ShellScreen used to rescale the
    // entire UI and re-request every icon at a size the device never renders.
    readonly property int _rawScreenHeight: Quickshell.screens.length > 0 ? Quickshell.screens[0].height : 0

    // { height, ready } — the folded state. `adopt()` returns the SAME object when
    // a reading is degenerate, so a bad report emits no change at all.
    property var _screen: ScreenScale.initial()

    function _adoptScreenHeight() {
        units._screen = ScreenScale.adopt(units._screen, units._rawScreenHeight);
    }

    on_RawScreenHeightChanged: units._adoptScreenHeight()
    Component.onCompleted: units._adoptScreenHeight()

    // Public, unchanged name — every existing consumer keeps reading this. Always
    // a usable number so layout arithmetic never divides by zero.
    readonly property int screenHeight: units._screen.height

    // FALSE until a real screen height has been observed, i.e. `screenHeight` is
    // still the layout-only placeholder and the whole scale is a guess.
    //
    // This is what stops the startup half of the icon flood. Seeding a plausible
    // height keeps first paint laid out, but on this device (1080 logical) the
    // placeholder is DOUBLE the real scale, so every icon requested before the
    // screen settles is requested at a size that is then thrown away and
    // re-requested — `AppIcon` sets `cache: false` (load-bearing for the #194
    // stale-texture bug), so that second wave is guaranteed. Consumers that issue
    // a sized request must gate on this, not just on a non-zero size. One-way:
    // once true it never returns to false, so a later DPMS/mode-set transient
    // cannot blank the UI's icons.
    readonly property bool screenReady: units._screen.ready === true

    // Floor on the derived unit. With the >=200 input floor in screenScale.js this
    // is unreachable in practice — it is a cheap structural guarantee that no
    // future arithmetic path can drive every icon size, radius and border to 0,
    // not a guard that fires today.
    readonly property int gridUnit: Math.max(8, Math.round(screenHeight / 40))

    readonly property int spacingXS: Math.round(gridUnit * 0.15)
    readonly property int spacingSM: Math.round(gridUnit * 0.30)
    readonly property int spacingMD: Math.round(gridUnit * 0.44)
    readonly property int spacingLG: Math.round(gridUnit * 0.59)
    readonly property int spacingXL: Math.round(gridUnit * 0.89)

    readonly property int radiusSM: Math.round(gridUnit * 0.15)
    readonly property int radiusMD: Math.round(gridUnit * 0.30)
    readonly property int radiusLG: Math.round(gridUnit * 0.44)
    readonly property int radiusXL: Math.round(gridUnit * 0.59)

    readonly property int borderThin: Math.max(1, Math.round(gridUnit * 0.037))
    readonly property int borderMedium: Math.max(2, Math.round(gridUnit * 0.056))
    readonly property int borderThick: Math.max(3, Math.round(gridUnit * 0.11))

    readonly property int iconSizeSM: Math.round(gridUnit * 0.59)
    readonly property int iconSizeMD: Math.round(gridUnit * 1.19)
    readonly property int iconSizeLG: Math.round(gridUnit * 2.22)
    readonly property int iconSizeXL: Math.round(gridUnit * 4.44)

    // Settings-panel chrome — previously hardcoded to the 4K reference (560/100/
    // 80 px). Expressed off gridUnit so the panel scales on non-4K screens; the
    // multipliers are chosen to land on the original pixel values at 2160p.
    readonly property int sidebarWidth: Math.round(gridUnit * 10.37)
    readonly property int settingsRowHeight: Math.round(gridUnit * 1.85)
    readonly property int settingsHintHeight: Math.round(gridUnit * 1.48)
}
