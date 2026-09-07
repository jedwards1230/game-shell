// Placeholder content on a real structure.
//
// What is placeholder: every pixel. Coloured rectangles, no theme, no data.
// What is NOT placeholder, and is the point of this file:
//
//   * Three SEPARATE toplevels -- base, overlay, toast -- as sibling Surfaces of
//     a non-visual root. They are siblings rather than nested Windows on purpose:
//     a Window declared inside a Window gets a transient parent (WM_TRANSIENT_FOR),
//     which is a different thing to gamescope than an independent toplevel.
//     docs/V2_DESIGN.md §5 requires drawer/QAM/toasts to be their own toplevels,
//     and this is where that becomes structural rather than a convention.
//   * One FocusRouter over a grid of FocusSlots, driven by D-pad keys, with a
//     slot that can be disabled live to show that focus re-homes instead of
//     stranding.
pragma ComponentBehavior: Bound

import QtQuick
import TvShell

QtObject {
    id: root

    // The one place the demo's disable-a-slot state lives. Bound into the grid
    // below so the "middle cell vanishes" case is reachable from the couch with
    // a single key.
    property bool middleEnabled: true
    property bool overlayOpen: false
    property bool toastVisible: false

    readonly property FocusRouter router: FocusRouter {}

    // ---- base -------------------------------------------------------------
    // The shell itself. STEAM_GAME=<appid>, written before this window maps.
    readonly property Surface base: Surface {
        id: baseSurface

        role: Surface.Base
        visible: true
        color: "#101014"
        title: "tv-shell"

        Item {
            anchors.fill: parent
            focus: true

            // The pad/keyboard entry point. Every directional key is one call
            // into the router; nothing here knows what is adjacent to what.
            Keys.onLeftPressed: root.router.move("left")
            Keys.onRightPressed: root.router.move("right")
            Keys.onUpPressed: root.router.move("up")
            Keys.onDownPressed: root.router.move("down")
            // Deliberately reachable in the spike: proves the re-home path on
            // hardware without a debugger.
            Keys.onSpacePressed: root.middleEnabled = !root.middleEnabled
            Keys.onMenuPressed: root.overlayOpen = !root.overlayOpen
            Keys.onTabPressed: root.toastVisible = !root.toastVisible

            Text {
                x: 48
                y: 40
                color: "#e8e8ef"
                font.pixelSize: 34
                text: "tv-shell v2 shim · focus=" + (root.router.currentId || "(none)") + "   ·   tagged=" + baseSurface.tagged
            }
            Text {
                x: 48
                y: 88
                color: "#8a8a99"
                font.pixelSize: 22
                text: "arrows: move   space: toggle the middle cell   menu: overlay   tab: toast"
            }

            Grid {
                x: 48
                y: 160
                columns: 3
                spacing: 24

                Repeater {
                    model: 9

                    FocusSlot {
                        id: cell

                        required property int index

                        slotId: "cell-" + cell.index
                        row: Math.floor(cell.index / 3)
                        column: cell.index % 3
                        // Cell 4 is the middle of the grid, so disabling it
                        // exercises both re-home and the R1 row skip when its
                        // whole row is emptied by the same toggle.
                        slotEnabled: cell.index === 4 ? root.middleEnabled : true
                        router: root.router

                        width: 240
                        height: 140
                        visible: true

                        Rectangle {
                            anchors.fill: parent
                            radius: 10
                            color: cell.slotEnabled ? "#1d1d28" : "#141419"
                            border.width: cell.current ? 4 : 1
                            border.color: cell.current ? "#c8102e" : "#33333f"

                            Text {
                                anchors.centerIn: parent
                                color: cell.slotEnabled ? "#e8e8ef" : "#4a4a55"
                                font.pixelSize: 26
                                text: cell.slotId
                            }
                        }
                    }
                }
            }
        }
    }

    // ---- overlay ----------------------------------------------------------
    // Drawer/QAM shape: takes keyboard and mouse WITHOUT displacing whatever is
    // on the base layer. STEAM_OVERLAY=1 + STEAM_INPUT_FOCUS=1, before map.
    readonly property Surface overlay: Surface {
        id: overlaySurface

        role: Surface.Overlay
        visible: root.overlayOpen
        width: 640
        height: 900
        color: "#0b0b0f"
        title: "tv-shell-overlay"

        Text {
            anchors.centerIn: parent
            color: "#e8e8ef"
            font.pixelSize: 28
            text: "overlay · tagged=" + overlaySurface.tagged
        }
    }

    // ---- toast ------------------------------------------------------------
    // Notification shape: composited over whatever is on screen and input-inert.
    // STEAM_OVERLAY=1 + STEAM_NOTIFICATION=1, and deliberately NO
    // STEAM_INPUT_FOCUS -- a toast that takes the pad mid-game is a bug.
    readonly property Surface toast: Surface {
        role: Surface.Toast
        visible: root.toastVisible
        width: 520
        height: 140
        color: "#16161d"
        title: "tv-shell-toast"

        Text {
            anchors.centerIn: parent
            color: "#e8e8ef"
            font.pixelSize: 24
            text: "toast"
        }
    }
}
