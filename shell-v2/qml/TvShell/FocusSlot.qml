// One addressable cell in a FocusRouter's graph.
//
// A slot declares three things and nothing else: WHERE it sits (`row`,
// `column`), WHETHER it can take focus now (`slotEnabled`), and WHO routes for
// it (`router`). It does not name a single neighbour. That is the whole
// contract -- compare v1's eight duck-typed members spread across seventeen
// files.
//
// `current` is the slot's read-only answer to "am I the focused cell", derived
// from the router rather than assigned by anyone. Content goes inside as
// ordinary children; a slot is a FocusScope so real Qt focus follows the model.
pragma ComponentBehavior: Bound

import QtQuick

FocusScope {
    id: slot

    // Stable identity. Must be unique within one router -- FocusRouter warns at
    // registration when it is not, which is as early as QML allows.
    required property string slotId

    // Where the slot sits. Rows and columns need not be contiguous: gaps are
    // meaningless to the router, which only ever compares them.
    required property int row
    required property int column

    // Whether this cell can hold focus right now. Flipping it to false while
    // focused is the case the whole model exists for: the router re-homes, and
    // no neighbour anywhere needs re-wiring.
    property bool slotEnabled: true

    required property FocusRouter router

    // Null-guarded because QML clears object references during teardown: at
    // shutdown the router can be destroyed before its slots, and an unguarded
    // dereference here turns an ordinary exit into a page of TypeErrors.
    readonly property bool current: slot.router !== null && slot.router.currentId === slot.slotId

    // Real Qt focus follows the model, never the other way round. The router is
    // the single writer of focus state; this is the one place it becomes an
    // actual activeFocus.
    onCurrentChanged: if (slot.current)
        slot.forceActiveFocus()

    // Any change to focusability -- including the container above going hidden --
    // republishes the graph. Both properties are watched because both feed
    // FocusRouter.cells().
    onSlotEnabledChanged: if (slot.router)
        slot.router.sync()
    onVisibleChanged: if (slot.router)
        slot.router.sync()

    Component.onCompleted: if (slot.router)
        slot.router.register(slot)
    // Same teardown guard as `current` above: a slot outliving its router by one
    // destruction step must not throw on the way out.
    Component.onDestruction: if (slot.router)
        slot.router.unregister(slot)
}
