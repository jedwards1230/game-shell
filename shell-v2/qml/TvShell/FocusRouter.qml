// The router half of the focus model. Owns `currentId`; slots read it.
//
// Nothing here decides anything -- every decision is a call into focusGraph.js,
// which is pure and headlessly tested. This file is glue: it keeps the live cell
// set in sync with the slots that exist right now, and it repairs `currentId`
// when the cell under it stops being focusable.
//
// The direction of the arrow is the design. A slot never learns who its
// neighbours are, and the router never holds a reference to "the item to focus
// next" -- it holds an id, recomputed on every move from the current set. There
// is no wiring to get wrong because there is no wiring.
pragma ComponentBehavior: Bound

import QtQuick
import "focusGraph.js" as FocusGraph

QtObject {
    id: router

    // Registered FocusSlot items. Written only by register()/unregister().
    property var slots: []

    // The focused cell's id, or "" when the graph is empty. This is the single
    // piece of focus state in the model.
    property string currentId: ""

    // Coordinates of the last cell we focused, kept so `rehome` still has
    // somewhere to measure from after that cell is gone.
    property int lastRow: 0
    property int lastColumn: 0

    // Emitted when the graph was well-formed but focus could not be placed --
    // i.e. nothing at all is focusable. Distinguished from an ordinary edge
    // no-op so a caller can decide whether that is expected (a genuinely empty
    // screen) or a bug.
    signal focusUnplaceable

    function cells(): var {
        const out = [];
        for (const slot of router.slots) {
            if (!slot)
                continue;
            out.push({
                "id": slot.slotId,
                "row": slot.row,
                "column": slot.column,
                // `visible` is included so a slot inside a hidden container
                // leaves the graph without every caller remembering to also
                // clear slotEnabled. Two ways to say "not now", one meaning.
                "focusable": slot.slotEnabled && slot.visible
            });
        }
        return out;
    }

    function register(slot: Item) {
        const next = router.slots.slice();
        next.push(slot);
        router.slots = next;
        // Log structural defects at the first moment anything can: QML gives no
        // compile-time check that ids are unique, so this is the substitute.
        const bad = FocusGraph.problems(router.cells());
        if (bad.length > 0)
            console.warn("FocusRouter: malformed focus graph:", bad.join("; "));
        if (router.currentId === "")
            router.focusInitial();
    }

    function unregister(slot: Item) {
        const next = router.slots.filter(s => s !== slot);
        if (next.length === router.slots.length)
            return;
        router.slots = next;
        router.sync();
    }

    function setCurrent(id: string) {
        router.currentId = id;
        for (const slot of router.slots) {
            if (slot && slot.slotId === id) {
                router.lastRow = slot.row;
                router.lastColumn = slot.column;
                break;
            }
        }
    }

    function focusInitial() {
        const id = FocusGraph.initial(router.cells());
        if (id === null) {
            router.currentId = "";
            router.focusUnplaceable();
            return;
        }
        router.setCurrent(id);
    }

    // Move focus one cell in `direction` ("left"|"right"|"up"|"down").
    // Returns true when focus moved. A false return is an ordinary edge stop,
    // not an error -- B/Back is the way out of a corner, not a wrap.
    function move(direction: string): bool {
        const id = FocusGraph.neighbour(router.cells(), router.currentId, direction);
        if (id === null)
            return false;
        router.setCurrent(id);
        return true;
    }

    // Re-place focus after the cell set changed. Called by every slot whenever
    // its focusability changes, and by unregister(). This is the function that
    // makes stranding structurally unreachable: if the cell under focus is gone,
    // focus goes to the nearest survivor, and only a completely empty graph can
    // leave it unplaced.
    function sync() {
        const live = router.cells();
        for (const cell of live) {
            if (cell.id === router.currentId && cell.focusable)
                return; // still valid, nothing to do
        }
        const id = FocusGraph.rehome(live, router.lastRow, router.lastColumn);
        if (id === null) {
            router.currentId = "";
            router.focusUnplaceable();
            return;
        }
        router.setCurrent(id);
    }
}
